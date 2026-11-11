#ifndef HENGEVM_XHCI_H
#define HENGEVM_XHCI_H

#include <pthread.h>

#include "pcie.h"

/* An xHCI USB host controller (xHCI 1.0, one interrupter, 32-byte contexts,
   INTx) with two USB HID devices: a boot keyboard on port 1 and a Magic
   Trackpad 2 on port 2: what Virtualization.framework gives a macOS guest as
   its "Virtual USB Keyboard", plus Apple's multitouch pad, so the guest's
   own gesture engine sees a real trackpad. Port 3 is empty until a game
   controller is in use (hv_xhci_gamepad), and then holds a DualShock 4. Port
   4 is empty until the pointer is put somewhere by position (hv_xhci_tablet),
   and then holds a USB tablet.
   Changes come from the host UI on any thread (hv_xhci_key, hv_xhci_touch,
   hv_xhci_gamepad, hv_xhci_tablet); the CPU thread delivers them (hv_xhci_poll). */
#define HV_XHCI_SLOTS 8
#define HV_XHCI_PORTS 4
#define HV_XHCI_DEVICES 4
#define HV_XHCI_INTRS 4
#define HV_XHCI_EPS 32 /* device context index 1..31 */
#define HV_XHCI_REPORTS 256

/* The Magic Trackpad 2's sensor, in its own units (a ~160 x 115 mm pad).
   hv_xhci_touch takes coordinates in these ranges. */
#define HV_TP_SENSOR_X 8134
#define HV_TP_SENSOR_Y 5206
#define HV_TP_FINGERS 10
#define HV_TP_REPORTS 64
#define HV_TP_REPORT_LEN (12 + 9 * HV_TP_FINGERS) /* report ID 2: 12-byte prefix, N x 9-byte fingers */

#define HV_PAD_REPORTS 64
#define HV_PAD_REPORT_LEN 64 /* the DualShock 4's input report over USB, report ID 1 */

#define HV_TAB_REPORTS 64
#define HV_TAB_REPORT_LEN 6 /* the tablet's: buttons, X, Y (16 bits each), wheel */

typedef struct HvXhciRing {
    uint64_t dequeue; /* guest PA of the next TRB */
    int ccs;          /* consumer cycle state */
} HvXhciRing;

typedef struct HvXhci {
    HvPciDev pci;
    HvMachine *m;
    /* operational */
    uint32_t usbcmd, usbsts, dnctrl, config;
    uint64_t dcbaap;
    HvXhciRing cmd;
    int cmd_running;
    uint32_t portsc[HV_XHCI_PORTS];
    /* interrupters and their event rings (macOS's driver wants at least three) */
    struct HvXhciIntr {
        uint32_t iman, imod, erstsz;
        uint64_t erstba, erdp;
        uint64_t seg_base;  /* current event ring segment */
        uint32_t seg_size, seg_index, index;
        int pcs;
    } intr[HV_XHCI_INTRS];
    /* device slots: -1 until the device on the slot's port is addressed */
    int slot_used[HV_XHCI_SLOTS + 1];
    struct HvXhciSlot {
        int dev;         /* 0 keyboard, 1 trackpad, 2 controller, 3 tablet, -1 none */
        int configured;
        HvXhciRing ep[HV_XHCI_EPS];
        int ep_running[HV_XHCI_EPS];
    } slots[HV_XHCI_SLOTS + 1];
    /* keyboard reports waiting for an IN transfer (8 bytes each) */
    pthread_mutex_t mu;
    uint8_t reports[HV_XHCI_REPORTS][8];
    uint32_t rhead, rtail;
    uint8_t state[8];      /* modifiers, reserved, six keys: the current report */
    uint64_t delivered;
    /* Magic Trackpad 2: the fingers on the pad and their reports (Apple
       multitouch format, report ID 2: 12-byte prefix plus 9 bytes a finger). */
    struct HvTpFinger {
        int in;
        int id;        /* the report's 4-bit touch ID, 1..15 */
        int x, y;      /* sensor coordinates, 0..HV_TP_SENSOR_X/Y */
        uint8_t state; /* the report's 3-bit state: 0 none, 3 near, 4 contact, 7 stop */
    } tp_fingers[HV_TP_FINGERS];
    int tp_button;     /* the pad's full press (report byte 1, bit 0) */
    uint32_t tp_ts;    /* the report timestamp, one per report */
    uint8_t tp_reports[HV_TP_REPORTS][HV_TP_REPORT_LEN];
    uint8_t tp_rlen[HV_TP_REPORTS];
    uint32_t tp_rhead, tp_rtail;
    int tp_mt;         /* the guest switched us to multitouch mode (feature report 2) */
    uint64_t tp_delivered;
    uint32_t tp_reqs;   /* control requests logged, capped */
    uint32_t tp_db;     /* doorbells logged, capped */
    uint32_t tp_multi_logs; /* multi-finger deliveries logged, capped */
    uint8_t tp_f1[8];   /* feature report 1: the header for the selected sensor register */
    int tp_f1_len;
    /* DualShock 4: whether it should be on the bus, its current input report and
       the reports waiting for an IN transfer. */
    int pad_on;
    uint8_t pad_state[HV_PAD_REPORT_LEN];
    uint8_t pad_reports[HV_PAD_REPORTS][HV_PAD_REPORT_LEN];
    uint32_t pad_rhead, pad_rtail;
    uint8_t pad_seq;    /* the report's 6-bit counter */
    uint64_t pad_delivered;
    uint32_t pad_reqs;  /* control requests logged, capped */
    /* The tablet: whether it should be on the bus, its current report and the
       reports waiting for an IN transfer. */
    int tab_on;
    uint8_t tab_state[HV_TAB_REPORT_LEN];
    uint8_t tab_reports[HV_TAB_REPORTS][HV_TAB_REPORT_LEN];
    uint32_t tab_rhead, tab_rtail;
    int tab_last_move;  /* the newest waiting report only moved the pointer: the next move may take its place */
    uint64_t tab_delivered;
    uint32_t tab_reqs;  /* control requests logged, capped */
} HvXhci;

HvXhci *hv_xhci_create(HvMachine *m, HvPcie *bus);
/* Any thread: a key went down or up (HID usage page 7 usage, e.g. 4 = A; 0xe0-0xe7 are modifiers). */
void hv_xhci_key(HvXhci *x, uint8_t usage, int down);
/* The same, by Linux input key code (what the host UI produces). */
void hv_xhci_key_linux(HvXhci *x, uint16_t code, int down);
/* Any thread: a Magic Trackpad 2 touch. phase: 0 down, 1 move, 2 up. id: a
   stable 1..15 for this touch; x, y in sensor units (HV_TP_SENSOR_*). Down and up
   report at once; a move only updates the finger, and hv_xhci_touch_frame commits
   one report for all the fingers that moved together. ts_ms is the touch sample's
   own clock (ms); the guest divides position deltas by timestamp deltas to get
   speed, so it must be the sample time, not the moment we happen to process it. */
void hv_xhci_touch(HvXhci *x, int id, int phase, int px, int py, uint32_t ts_ms);
/* Any thread: send one frame with every finger's current state (ends a move batch),
   stamped with the touch sample's own clock (ms). */
void hv_xhci_touch_frame(HvXhci *x, uint32_t ts_ms);
/* Any thread: the pad's full press (a click), down or up. */
void hv_xhci_touch_button(HvXhci *x, int down);
/* Any thread: the state of the game controller, a DualShock 4 that the first call
   plugs into port 3. buttons, by bit (SDL's gamepad button order): 0 south (cross),
   1 east (circle), 2 west (square), 3 north (triangle), 4 back (share), 5 guide
   (PS), 6 start (options), 7 left stick, 8 right stick, 9 left shoulder, 10 right
   shoulder, 11-14 d-pad up, down, left, right. axes: 0-3 left x, left y, right x,
   right y (-32768..32767, y grows downwards), 4-5 left and right trigger
   (0..32767). Only a change makes a report. NULL axes: the controller is gone,
   unplug it. */
void hv_xhci_gamepad(HvXhci *x, uint32_t buttons, const int16_t axes[6]);
/* Any thread: where the pointer is and which of its buttons are down, through a
   USB tablet that the first call plugs into port 4. buttons, by bit: 0 left,
   1 right, 2 middle. px, py: 0..32767 across the main display, from its top left.
   Only a change makes a report. px < 0: unplug it. */
void hv_xhci_tablet(HvXhci *x, int buttons, int px, int py);
/* CPU thread: hand waiting reports to the guest. */
void hv_xhci_poll(HvXhci *x);

#endif
