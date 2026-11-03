#pragma once

#include <cstdint>

/// Whether this firmware lets an app touch peripheral registers, asked in an order that cannot itself fault.
namespace IsolationGate
{

enum class Verdict : uint8_t { Allowed, Unprivileged, MpuEnabled, TrustZone };

/// Armv8-M `CONTROL.nPRIV`, `MPU_CTRL` and its `ENABLE`; `FLASH_OPTR` and its `TZEN` (RM0456 7.9.13).
constexpr uint32_t kControlNPriv  = 1u;
constexpr uint32_t kMpuCtrl       = 0xE000ED94u;
constexpr uint32_t kMpuCtrlEnable = 1u;
constexpr uint32_t kFlashOptr     = 0x40022040u;
constexpr uint32_t kOptrTzen      = 1u << 31;

constexpr Verdict decide(uint32_t control, uint32_t mpuCtrl, uint32_t flashOptr)
{
    if ((control & kControlNPriv) != 0u)    return Verdict::Unprivileged;
    if ((mpuCtrl & kMpuCtrlEnable) != 0u)   return Verdict::MpuEnabled;
    if ((flashOptr & kOptrTzen) != 0u)      return Verdict::TrustZone;
    return Verdict::Allowed;
}

Verdict check();

}
