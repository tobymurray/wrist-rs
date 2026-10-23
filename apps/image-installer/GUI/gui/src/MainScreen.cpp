#include "gui/MainScreen.hpp"

#include <cstdio>

#include "SDK/GUI/Button.hpp"
#include "SDK/GUI/Color.hpp"
#include "SDK/GUI/LVGL/Draw.hpp"

namespace Draw  = SDK::LVGL::Draw;
namespace Color = SDK::GUI::Color;

using Buttons = SDK::LVGL::Buttons;
using Input   = ConfirmFlow::Input;
using Phase   = ConfirmFlow::Phase;
using State   = InstallerMessage::State;

namespace
{
/// The port runs LVGL's timers on the kernel's 10 Hz GUI tick, so this is as fine as it gets.
constexpr uint32_t kTickMs = 100;

constexpr int32_t kScreen = 240;
constexpr int32_t kTextX  = 30;
constexpr int32_t kTextW  = 180;
}

MainScreen::MainScreen(Model& model)
    : mModel(model)
{
    mRoot = lv_obj_create(nullptr);
    Draw::applyScreen(mRoot);
    lv_obj_add_event_cb(mRoot, &MainScreen::keyEventCb, LV_EVENT_KEY, this);

    // The verdict.
    mInfo = Draw::container(mRoot, 0, 0, kScreen, kScreen);
    Draw::label(mInfo, &lv_font_montserrat_14, "Install wrist-rs", kTextX, 30, kTextW,
                LV_TEXT_ALIGN_CENTER, Color::TEAL);
    mHeadline = Draw::label(mInfo, &lv_font_montserrat_28, "", kTextX, 56, kTextW);
    mCaption  = Draw::label(mInfo, &lv_font_montserrat_14, "", kTextX, 94, kTextW,
                            LV_TEXT_ALIGN_CENTER, Color::GRAY);
    mDetail   = Draw::label(mInfo, &lv_font_montserrat_14, "", kTextX, 122, kTextW);
    lv_label_set_long_mode(mDetail, LV_LABEL_LONG_WRAP);
    mHint     = Draw::label(mInfo, &lv_font_montserrat_14, "", kTextX, 192, kTextW,
                            LV_TEXT_ALIGN_CENTER, Color::GRAY);

    // The hold, laid out as the stock activities' hold to save: prompt, countdown, ring.
    mHold = Draw::container(mRoot, 0, 0, kScreen, kScreen);
    mRing = std::make_unique<SDK::LVGL::TimerRing>(mHold);
    mRing->setColor(Color::GREEN);
    Draw::label(mHold, &lv_font_montserrat_28, "Hold to\ninstall", kTextX, 46, kTextW);
    mNumber  = Draw::label(mHold, &lv_font_montserrat_48, "3", kTextX, 112, kTextW);
    mHoldCrc = Draw::label(mHold, &lv_font_montserrat_14, "", kTextX, 176, kTextW,
                           LV_TEXT_ALIGN_CENTER, Color::GRAY);
    Draw::setHidden(mHold, true);

    mButtons = std::make_unique<Buttons>(mRoot);

    mTick = lv_timer_create(&MainScreen::tickCb, kTickMs, this);

    mModel.bind(this);
    onVerdict(mModel.verdict());
}

MainScreen::~MainScreen()
{
    mModel.bind(nullptr);
    if (mTick != nullptr) {
        lv_timer_delete(mTick);
    }
    lv_obj_delete(mRoot);
}

void MainScreen::keyEventCb(lv_event_t* e)
{
    auto* self = static_cast<MainScreen*>(lv_event_get_user_data(e));
    self->onKey(static_cast<uint8_t>(lv_event_get_key(e)));
}

void MainScreen::tickCb(lv_timer_t* t)
{
    auto* self = static_cast<MainScreen*>(lv_timer_get_user_data(t));
    self->apply(Input::Tick);
}

void MainScreen::onKey(uint8_t code)
{
    namespace Btn = SDK::GUI::Button;
    switch (code) {
        case Btn::R1_PRESS:   apply(Input::R1Press); break;
        case Btn::R1_RELEASE: apply(Input::R1Release); break;
        case Btn::R2:         apply(Input::R2Click); break;
        default:              break;
    }
}

void MainScreen::onVerdict(const InstallerMessage::Verdict& verdict)
{
    switch (verdict.state) {
        case State::Checking:   render(); break;
        case State::Ready:      apply(Input::ReportReady); break;
        case State::Installing: apply(Input::ReportInstalling); break;
        default:                apply(Input::ReportBlocked); break;
    }
}

void MainScreen::onSuspend()
{
    // Button codes stop while suspended, so a release would never arrive: cancel instead.
    apply(Input::Suspend);
}

void MainScreen::apply(Input input)
{
    const Phase before = mFlow.phase();
    switch (mFlow.handle(input, mModel.nowMs())) {
        case ConfirmFlow::Action::Confirm:
            // The CRC on screen is the one the service checks the confirm against.
            mModel.confirm(mModel.verdict().crc);
            break;
        case ConfirmFlow::Action::Exit:
            mModel.exitApp();
            return;
        case ConfirmFlow::Action::None:
            break;
    }
    if (input != Input::Tick || mFlow.phase() != before || mFlow.phase() == Phase::Holding) {
        render();
    }
}

void MainScreen::show(const char* headline, const char* detail, const char* hint)
{
    lv_label_set_text(mHeadline, headline);
    lv_label_set_text(mDetail, detail);
    lv_label_set_text(mHint, hint);
}

void MainScreen::render()
{
    const InstallerMessage::Verdict& v = mModel.verdict();
    char crc[12];
    std::snprintf(crc, sizeof(crc), "%08lX", static_cast<unsigned long>(v.crc));

    const bool holding = mFlow.phase() == Phase::Holding;
    Draw::setHidden(mInfo, holding);
    Draw::setHidden(mHold, !holding);
    if (holding) {
        const uint32_t now    = mModel.nowMs();
        const uint32_t number = mFlow.countdown(now);
        if (number != mShown) {
            mShown = number;
            lv_label_set_text_fmt(mNumber, "%u", static_cast<unsigned>(number));
        }
        mRing->setProgress(mFlow.held(now) * 1000u / ConfirmFlow::kHoldMs);
        lv_label_set_text(mHoldCrc, crc);
        // No back hint: letting go of R1 cancels.
        mButtons->set(Buttons::NONE, Buttons::NONE, Buttons::GREEN, Buttons::NONE);
        return;
    }
    mShown = 0;
    lv_label_set_text(mCaption, "");

    switch (mFlow.phase()) {
        case Phase::Waiting:
            show("Checking", "Reading image.bin and testing flash. Nothing is replaced.", "Back to exit");
            mButtons->set(Buttons::NONE, Buttons::NONE, Buttons::NONE, Buttons::WHITE);
            return;

        case Phase::Ready:
            lv_label_set_text(mCaption, "image CRC-32");
            show(crc, "Dry run passed. Check this against INSTALL-CRC.txt.", "Hold Select to install");
            mButtons->set(Buttons::NONE, Buttons::NONE, Buttons::AMBER, Buttons::WHITE);
            return;

        case Phase::Sent:
            show("Installing", "Do not interrupt power. The watch restarts into wrist-rs.", "");
            mButtons->set(Buttons::NONE, Buttons::NONE, Buttons::NONE, Buttons::NONE);
            return;

        case Phase::Holding:
        case Phase::Blocked:
            break;
    }

    char headline[24];
    mButtons->set(Buttons::NONE, Buttons::NONE, Buttons::NONE, Buttons::WHITE);
    switch (v.state) {
        case State::Isolated:
            show("Refused", "The firmware isolates apps. Nothing was attempted.", "Back to exit");
            break;
        case State::NoImage:
            show("No image", "image.bin is missing, empty or unreadable.", "Back to exit");
            break;
        case State::UnlockFailed:
            std::snprintf(headline, sizeof(headline), "Unlock %ld", static_cast<long>(v.code));
            show(headline, "The flash unlock self-test failed. Nothing is replaced.", "Back to exit");
            break;
        case State::FlashTestFailed:
            std::snprintf(headline, sizeof(headline), "Flash %02lX",
                          static_cast<unsigned long>(v.code & 0xFF));
            show(headline, "The flash self-test failed. Nothing is replaced.", "Back to exit");
            break;
        default:
            std::snprintf(headline, sizeof(headline), "Refused %ld", static_cast<long>(v.code));
            show(headline, "The stock firmware is not replaced. Keep install.log and stage.log.",
                 "Back to exit");
            break;
    }
}
