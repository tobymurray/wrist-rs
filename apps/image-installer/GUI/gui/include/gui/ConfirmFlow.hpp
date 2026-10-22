#pragma once

#include <cstdint>

/**
 * The on-screen confirm, kept free of LVGL so its rules are checked at compile time
 * (ConfirmFlowChecks.cpp).
 *
 * It is the stock activities' hold to save: on the ready screen, R1 pressed and held for
 * `kHoldMs` sends the confirm, with a countdown on screen. Letting go, R2, or leaving the
 * screen before then cancels. The hold has to start with a press on the ready screen, so a
 * press that began before the dry run finished, or before the app opened, never counts.
 */
class ConfirmFlow
{
public:
    enum class Phase : uint8_t
    {
        /// No report from the service yet.
        Waiting,
        /// The service reported something other than a ready dry run.
        Blocked,
        Ready,
        Holding,
        /// The confirm has gone; buttons do nothing until the service reports.
        Sent,
    };

    enum class Input : uint8_t
    {
        ReportReady,
        ReportBlocked,
        ReportInstalling,
        R1Press,
        R1Release,
        R2Click,
        Tick,
        Suspend,
    };

    enum class Action : uint8_t
    {
        None,
        Confirm,
        Exit,
    };

    /// Twice the stock activities' 1.5 s: replacing the firmware is harder to undo than saving.
    static constexpr uint32_t kHoldMs = 3000;

    constexpr Action handle(Input input, uint32_t nowMs)
    {
        switch (input) {
            case Input::ReportReady:
                if (mPhase == Phase::Waiting || mPhase == Phase::Blocked) {
                    enter(Phase::Ready, nowMs);
                }
                return Action::None;

            case Input::ReportBlocked:
                enter(Phase::Blocked, nowMs);
                return Action::None;

            case Input::ReportInstalling:
                enter(Phase::Sent, nowMs);
                return Action::None;

            case Input::R1Press:
                if (mPhase == Phase::Ready) {
                    enter(Phase::Holding, nowMs);
                }
                return Action::None;

            case Input::R1Release:
            case Input::Suspend:
                if (mPhase == Phase::Holding) {
                    enter(Phase::Ready, nowMs);
                }
                return Action::None;

            case Input::R2Click:
                if (mPhase == Phase::Holding) {
                    enter(Phase::Ready, nowMs);
                    return Action::None;
                }
                return (mPhase == Phase::Sent) ? Action::None : Action::Exit;

            case Input::Tick:
                if (mPhase == Phase::Holding && nowMs - mSince >= kHoldMs) {
                    enter(Phase::Sent, nowMs);
                    return Action::Confirm;
                }
                return Action::None;
        }
        return Action::None;
    }

    constexpr Phase phase() const { return mPhase; }

    /// How long R1 has been held, up to `kHoldMs`; 0 unless holding.
    constexpr uint32_t held(uint32_t nowMs) const
    {
        if (mPhase != Phase::Holding) {
            return 0;
        }
        const uint32_t elapsed = nowMs - mSince;
        return (elapsed < kHoldMs) ? elapsed : kHoldMs;
    }

    /// The countdown's number while holding: 3, 2, then 1; the confirm goes before 0.
    constexpr uint32_t countdown(uint32_t nowMs) const
    {
        const uint32_t step = kHoldMs / 3;
        const uint32_t left = 3 - held(nowMs) / step;
        return (left < 1) ? 1 : left;
    }

private:
    constexpr void enter(Phase phase, uint32_t nowMs)
    {
        mPhase = phase;
        mSince = nowMs;
    }

    Phase    mPhase = Phase::Waiting;
    uint32_t mSince = 0;
};
