// The confirm's rules, checked by the compiler on every build of the GUI.
#include "gui/ConfirmFlow.hpp"

namespace
{

using Phase  = ConfirmFlow::Phase;
using Input  = ConfirmFlow::Input;
using Action = ConfirmFlow::Action;

constexpr uint32_t kHold = ConfirmFlow::kHoldMs;

constexpr ConfirmFlow ready()
{
    ConfirmFlow f;
    f.handle(Input::ReportReady, 0);
    return f;
}

constexpr ConfirmFlow holding(uint32_t pressedAt)
{
    ConfirmFlow f = ready();
    f.handle(Input::R1Press, pressedAt);
    return f;
}

// A press held for the full time confirms once, and only then.
static_assert([] {
    ConfirmFlow f = holding(200);
    return f.handle(Input::Tick, 200 + kHold - 1) == Action::None && f.phase() == Phase::Holding &&
           f.handle(Input::Tick, 200 + kHold) == Action::Confirm && f.phase() == Phase::Sent &&
           f.handle(Input::Tick, 99999) == Action::None;
}());

// The countdown reads 3, 2, 1 and never 0.
static_assert([] {
    ConfirmFlow f = holding(0);
    return f.countdown(0) == 3 && f.countdown(kHold / 3) == 2 && f.countdown(2 * kHold / 3) == 1 &&
           f.countdown(kHold - 1) == 1 && f.countdown(kHold) == 1 && ready().countdown(0) == 3;
}());

// Letting go early cancels, and the next hold starts again from zero.
static_assert([] {
    ConfirmFlow f = holding(200);
    f.handle(Input::R1Release, 2000);
    const bool cancelled = f.phase() == Phase::Ready;
    f.handle(Input::R1Press, 2100);
    return cancelled && f.handle(Input::Tick, 200 + kHold) == Action::None &&
           f.held(2100 + 500) == 500 && f.handle(Input::Tick, 2100 + kHold) == Action::Confirm;
}());

// R2 and leaving the screen cancel a hold; R2 exits only when nothing is held or sent.
static_assert([] {
    ConfirmFlow a = holding(200);
    ConfirmFlow b = holding(200);
    a.handle(Input::R2Click, 300);
    b.handle(Input::Suspend, 300);
    return a.phase() == Phase::Ready && b.phase() == Phase::Ready &&
           b.handle(Input::Tick, 200 + kHold) == Action::None &&
           a.handle(Input::R2Click, 400) == Action::Exit;
}());

// A press from before the ready screen, still held when it appears, is not a hold.
static_assert([] {
    ConfirmFlow f;
    f.handle(Input::R1Press, 0);
    f.handle(Input::ReportReady, 100);
    return f.handle(Input::Tick, 100 + 10 * kHold) == Action::None && f.phase() == Phase::Ready;
}());

// Nothing is held before the service reports a ready dry run, or after it reports otherwise.
static_assert([] {
    ConfirmFlow waiting;
    waiting.handle(Input::R1Press, 0);
    ConfirmFlow blocked;
    blocked.handle(Input::ReportBlocked, 0);
    blocked.handle(Input::R1Press, 0);
    ConfirmFlow late = holding(200);
    late.handle(Input::ReportBlocked, 300);
    return waiting.phase() == Phase::Waiting && blocked.phase() == Phase::Blocked &&
           late.handle(Input::Tick, 200 + kHold) == Action::None;
}());

// Once the confirm has gone, R2 does not leave a screen that says it is installing.
static_assert([] {
    ConfirmFlow f = holding(200);
    f.handle(Input::Tick, 200 + kHold);
    f.handle(Input::ReportInstalling, 3300);
    return f.handle(Input::R2Click, 3400) == Action::None && f.phase() == Phase::Sent;
}());

// The millisecond clock may wrap during a hold.
static_assert([] {
    ConfirmFlow f = holding(0xFFFFFF00u);
    return f.handle(Input::Tick, kHold - 0x100u - 1) == Action::None &&
           f.handle(Input::Tick, kHold) == Action::Confirm;
}());

}
