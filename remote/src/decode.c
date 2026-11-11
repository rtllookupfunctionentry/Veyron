/* FFmpeg behind four calls, so the Rust side never sees libavcodec's structures (their layout changes with
   every major version). Decodes H.264 or HEVC Annex-B, on the GPU where the system offers it, and hands out
   NV12 or I420 pictures in ordinary memory. */
#include <libavcodec/avcodec.h>
#include <libavutil/hwcontext.h>
#include <libavutil/pixdesc.h>
#include <string.h>

typedef struct HdFrame {
    const uint8_t *data[3];
    int linesize[3];
    int width, height;
    int planes;     /* 2: NV12 (Y, interleaved UV); 3: I420 (Y, U, V) */
    int bt601;      /* the matrix to turn it into RGB with: 0 BT.709, 1 BT.601 */
    int full_range; /* 0: 16-235, 1: 0-255 */
} HdFrame;

typedef void (*HdOnFrame)(void *user, const HdFrame *f);

typedef struct HdDecoder {
    AVCodecContext *ctx;
    AVCodecParserContext *parser;
    AVPacket *pkt;
    AVFrame *frame, *sw;
    enum AVPixelFormat hw_fmt;
    char name[64];
} HdDecoder;

static enum AVPixelFormat pick_format(AVCodecContext *c, const enum AVPixelFormat *offered) {
    HdDecoder *d = c->opaque;
    for (const enum AVPixelFormat *p = offered; *p != AV_PIX_FMT_NONE; p++)
        if (*p == d->hw_fmt) return *p;
    /* The GPU declined this stream: the first software format, and say so in the name. */
    snprintf(d->name, sizeof d->name, "%s (software)", c->codec->name);
    return offered[0];
}

/* The first hardware decoder that opens, the platform's own first. */
static void try_hardware(HdDecoder *d, const AVCodec *codec) {
    static const enum AVHWDeviceType order[] = {AV_HWDEVICE_TYPE_VAAPI, AV_HWDEVICE_TYPE_D3D11VA, AV_HWDEVICE_TYPE_VIDEOTOOLBOX,
                                                AV_HWDEVICE_TYPE_VULKAN, AV_HWDEVICE_TYPE_DXVA2, AV_HWDEVICE_TYPE_VDPAU, AV_HWDEVICE_TYPE_CUDA};
    av_log_set_level(AV_LOG_QUIET); /* each kind this system lacks would otherwise complain */
    for (size_t t = 0; t < sizeof order / sizeof order[0]; t++) {
        for (int i = 0;; i++) {
            const AVCodecHWConfig *cfg = avcodec_get_hw_config(codec, i);
            if (!cfg) break;
            if (!(cfg->methods & AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX) || cfg->device_type != order[t]) continue;
            AVBufferRef *dev = NULL;
            if (av_hwdevice_ctx_create(&dev, order[t], NULL, NULL, 0) < 0) break;
            d->ctx->hw_device_ctx = dev;
            d->hw_fmt = cfg->pix_fmt;
            d->ctx->get_format = pick_format;
            snprintf(d->name, sizeof d->name, "%s (%s)", codec->name, av_hwdevice_get_type_name(order[t]));
            av_log_set_level(AV_LOG_ERROR);
            return;
        }
    }
    av_log_set_level(AV_LOG_ERROR);
}

void hd_close(HdDecoder *d) {
    if (!d) return;
    if (d->parser) av_parser_close(d->parser);
    avcodec_free_context(&d->ctx);
    av_packet_free(&d->pkt);
    av_frame_free(&d->frame);
    av_frame_free(&d->sw);
    av_free(d);
}

/* hevc: 0 H.264, 1 HEVC. hardware: try the GPU first. parse: the bytes come as an unbroken stream that has
   to be cut into pictures here (which costs a frame of delay); 0 when each hd_feed is one whole picture. */
HdDecoder *hd_open(int hevc, int hardware, int parse) {
    const AVCodec *codec = avcodec_find_decoder(hevc ? AV_CODEC_ID_HEVC : AV_CODEC_ID_H264);
    if (!codec) return NULL;
    HdDecoder *d = av_mallocz(sizeof *d);
    if (!d) return NULL;
    d->hw_fmt = AV_PIX_FMT_NONE;
    d->ctx = avcodec_alloc_context3(codec);
    d->pkt = av_packet_alloc();
    d->frame = av_frame_alloc();
    d->sw = av_frame_alloc();
    if (!d->ctx || !d->pkt || !d->frame || !d->sw) goto fail;
    d->ctx->opaque = d;
    d->ctx->flags |= AV_CODEC_FLAG_LOW_DELAY; /* a picture out for every picture in: the stream has no reordering */
    d->ctx->thread_count = 0;
    d->ctx->thread_type = FF_THREAD_SLICE; /* frame threads would hold pictures back */
    snprintf(d->name, sizeof d->name, "%s (software)", codec->name);
    if (hardware) try_hardware(d, codec);
    if (avcodec_open2(d->ctx, codec, NULL) < 0) goto fail;
    if (parse && !(d->parser = av_parser_init(codec->id))) goto fail;
    return d;
fail:
    hd_close(d);
    return NULL;
}

const char *hd_name(const HdDecoder *d) { return d->name; }

static int drain(HdDecoder *d, HdOnFrame on_frame, void *user) {
    int errors = 0;
    for (;;) {
        int r = avcodec_receive_frame(d->ctx, d->frame);
        if (r == AVERROR(EAGAIN) || r == AVERROR_EOF) break;
        if (r < 0) { errors++; break; }
        AVFrame *f = d->frame;
        if (f->format == d->hw_fmt && d->hw_fmt != AV_PIX_FMT_NONE) {
            av_frame_unref(d->sw);
            if (av_hwframe_transfer_data(d->sw, f, 0) < 0) { errors++; av_frame_unref(f); continue; }
            d->sw->colorspace = f->colorspace;
            d->sw->color_range = f->color_range;
            f = d->sw;
        }
        HdFrame out = {.width = f->width, .height = f->height};
        if (f->format == AV_PIX_FMT_NV12) out.planes = 2;
        else if (f->format == AV_PIX_FMT_YUV420P || f->format == AV_PIX_FMT_YUVJ420P) out.planes = 3;
        if (out.planes) {
            for (int i = 0; i < out.planes; i++) out.data[i] = f->data[i], out.linesize[i] = f->linesize[i];
            out.full_range = f->color_range == AVCOL_RANGE_JPEG || f->format == AV_PIX_FMT_YUVJ420P;
            /* Unmarked streams: HD is BT.709 by convention, smaller is BT.601. */
            out.bt601 = f->colorspace == AVCOL_SPC_BT470BG || f->colorspace == AVCOL_SPC_SMPTE170M ||
                        (f->colorspace == AVCOL_SPC_UNSPECIFIED && f->height < 720);
            on_frame(user, &out);
        } else {
            errors++; /* a pixel format this client does not draw (10-bit, 4:4:4) */
        }
        av_frame_unref(d->frame);
        av_frame_unref(d->sw);
    }
    return errors;
}

static int decode_packet(HdDecoder *d, const uint8_t *data, int len, HdOnFrame on_frame, void *user) {
    if (av_new_packet(d->pkt, len) < 0) return 1; /* a copy with the padding the bitstream readers need */
    memcpy(d->pkt->data, data, (size_t)len);
    int errors = avcodec_send_packet(d->ctx, d->pkt) < 0;
    av_packet_unref(d->pkt);
    return errors + drain(d, on_frame, user);
}

/* Returns how many pictures failed to decode (0 when all went well): the caller asks the phone for a keyframe. */
int hd_feed(HdDecoder *d, const uint8_t *data, int len, HdOnFrame on_frame, void *user) {
    if (!d->parser) return len > 0 ? decode_packet(d, data, len, on_frame, user) : 0;
    int errors = 0;
    while (len > 0) {
        uint8_t *out = NULL;
        int out_len = 0;
        int used = av_parser_parse2(d->parser, d->ctx, &out, &out_len, data, len, AV_NOPTS_VALUE, AV_NOPTS_VALUE, 0);
        if (used < 0) return errors + 1;
        data += used;
        len -= used;
        if (out_len > 0) errors += decode_packet(d, out, out_len, on_frame, user);
    }
    return errors;
}
