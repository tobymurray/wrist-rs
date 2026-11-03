#include "IsolationGate.hpp"

namespace IsolationGate
{

Verdict check()
{
    uint32_t control;
    asm volatile("mrs %0, control" : "=r"(control));
    if ((control & kControlNPriv) != 0u) {
        return Verdict::Unprivileged;
    }

    const uint32_t mpuCtrl = *reinterpret_cast<volatile const uint32_t*>(kMpuCtrl);
    if ((mpuCtrl & kMpuCtrlEnable) != 0u) {
        return Verdict::MpuEnabled;
    }

    const uint32_t flashOptr = *reinterpret_cast<volatile const uint32_t*>(kFlashOptr);
    return decide(control, mpuCtrl, flashOptr);
}

}
