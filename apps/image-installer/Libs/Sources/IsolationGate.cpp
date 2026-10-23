#include "IsolationGate.hpp"

namespace IsolationGate
{

Verdict check()
{
    uint32_t control;
    asm volatile("mrs %0, control" : "=r"(control));
    if ((control & 1u) != 0u) {
        return Verdict::Unprivileged;
    }

    const uint32_t mpuCtrl = *reinterpret_cast<volatile const uint32_t*>(0xE000ED94u);
    if ((mpuCtrl & 1u) != 0u) {
        return Verdict::MpuEnabled;
    }

    const uint32_t flashOptr = *reinterpret_cast<volatile const uint32_t*>(0x40022040u);
    return decide(control, mpuCtrl, flashOptr);
}

}
