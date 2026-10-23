/* The partitions of ../wrist-loader/LAYOUT.md; its memory.x must agree. */
MEMORY
{
  LOADER           : ORIGIN = 0x08060000, LENGTH = 32K
  FLASH            : ORIGIN = 0x08068000, LENGTH = 512K
  DFU              : ORIGIN = 0x080E8000, LENGTH = 520K
  BOOTLOADER_STATE : ORIGIN = 0x0816A000, LENGTH = 8K
  /* SRAM1. SRAM3 stages updates (src/update.rs), and SRAM5's top page holds the stack. */
  RAM              : ORIGIN = 0x20000000, LENGTH = 768K
}

__bootloader_state_start = ORIGIN(BOOTLOADER_STATE) - 0x08000000;
__bootloader_state_end   = ORIGIN(BOOTLOADER_STATE) + LENGTH(BOOTLOADER_STATE) - 0x08000000;
__bootloader_dfu_start   = ORIGIN(DFU) - 0x08000000;
__bootloader_dfu_end     = ORIGIN(DFU) + LENGTH(DFU) - 0x08000000;

/* Below the loader's mailbox and the failure record: HARDWARE.md, "Boot handoff". */
_stack_start = 0x2026FFE0;
