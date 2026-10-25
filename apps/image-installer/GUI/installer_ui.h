/*
 * The installer's screen, implemented in crates/wrist-installer-ui/src/ffi.rs.
 */
#ifndef WRIST_INSTALLER_UI_H
#define WRIST_INSTALLER_UI_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Bumped with any change to this header; a stale library is refused. */
#define WRIST_INSTALLER_UI_ABI 1u

#define WRIST_INSTALLER_UI_R1_PRESS   0u
#define WRIST_INSTALLER_UI_R1_RELEASE 1u
#define WRIST_INSTALLER_UI_R2_CLICK   2u
#define WRIST_INSTALLER_UI_TICK       3u
#define WRIST_INSTALLER_UI_SUSPEND    4u

/* What to do after an event, as bits. */
#define WRIST_INSTALLER_UI_REDRAW  1u
#define WRIST_INSTALLER_UI_CONFIRM 2u
#define WRIST_INSTALLER_UI_EXIT    4u

uint32_t wrist_installer_ui_abi(void);

/* The service's report: InstallerMessage::State's value, its code and the image's CRC. */
uint32_t wrist_installer_ui_report(uint8_t state, int32_t code, uint32_t crc, uint32_t now_ms);

/* With WRIST_INSTALLER_UI_CONFIRM, *confirm_crc is the CRC the confirm names. */
uint32_t wrist_installer_ui_input(uint8_t input, uint32_t now_ms, uint32_t* confirm_crc);

void wrist_installer_ui_render(uint8_t* buf, uint32_t len, uint16_t width, uint16_t height,
                               uint32_t now_ms);

/* Defined by the shell: a Rust panic, at file:line. */
void wrist_installer_ui_panicked(const uint8_t* file, uint32_t len, uint32_t line)
    __attribute__((noreturn));

#ifdef __cplusplus
}
#endif

#endif
