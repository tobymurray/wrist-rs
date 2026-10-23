#pragma once

#include <cstdint>

/// Whether this firmware lets an app touch peripheral registers, asked in an order that cannot itself fault.
namespace IsolationGate
{

enum class Verdict : uint8_t { Allowed, Unprivileged, MpuEnabled, TrustZone };

constexpr Verdict decide(uint32_t control, uint32_t mpuCtrl, uint32_t flashOptr)
{
    if ((control & 1u) != 0u)         return Verdict::Unprivileged;
    if ((mpuCtrl & 1u) != 0u)         return Verdict::MpuEnabled;
    if (((flashOptr >> 31) & 1u) != 0u) return Verdict::TrustZone;
    return Verdict::Allowed;
}

Verdict check();

}
