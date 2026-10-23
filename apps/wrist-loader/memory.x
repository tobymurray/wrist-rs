/* Must agree with apps/standalone-boot/memory.x; LAYOUT.md describes it. */
MEMORY
{
  FLASH            : ORIGIN = 0x08060000, LENGTH = 32K
  ACTIVE           : ORIGIN = 0x08068000, LENGTH = 512K
  DFU              : ORIGIN = 0x080E8000, LENGTH = 520K
  BOOTLOADER_STATE : ORIGIN = 0x0816A000, LENGTH = 8K
  RAM              : ORIGIN = 0x20000000, LENGTH = 0x270000
}

/* embassy-boot takes offsets from bank 1's base. */
__bootloader_state_start  = ORIGIN(BOOTLOADER_STATE) - 0x08000000;
__bootloader_state_end    = ORIGIN(BOOTLOADER_STATE) + LENGTH(BOOTLOADER_STATE) - 0x08000000;
__bootloader_active_start = ORIGIN(ACTIVE) - 0x08000000;
__bootloader_active_end   = ORIGIN(ACTIVE) + LENGTH(ACTIVE) - 0x08000000;
__bootloader_dfu_start    = ORIGIN(DFU) - 0x08000000;
__bootloader_dfu_end      = ORIGIN(DFU) + LENGTH(DFU) - 0x08000000;

/* Below the words the vendor bootloader's handoff reads and the mailbox uses
 * (HARDWARE.md, "Boot handoff"). */
_stack_start = ORIGIN(RAM) + LENGTH(RAM) - 32;
