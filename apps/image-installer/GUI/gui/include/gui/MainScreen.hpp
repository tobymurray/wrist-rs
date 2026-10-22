#pragma once

#include <cstdint>
#include <memory>

#include "lvgl.h"

#include "SDK/GUI/LVGL/Buttons.hpp"
#include "SDK/GUI/LVGL/TimerRing.hpp"

#include "gui/ConfirmFlow.hpp"
#include "gui/Model.hpp"

/// The installer's one screen: the dry run's verdict and the image's CRC, and over it, while
/// R1 is held, the stock activities' hold-to-confirm countdown.
class MainScreen : public ModelListener
{
public:
    explicit MainScreen(Model& model);
    ~MainScreen() override;

    MainScreen(const MainScreen&)            = delete;
    MainScreen& operator=(const MainScreen&) = delete;

    lv_obj_t* root() const { return mRoot; }

    void onVerdict(const InstallerMessage::Verdict& verdict) override;
    void onSuspend() override;

private:
    static void keyEventCb(lv_event_t* e);
    static void tickCb(lv_timer_t* t);

    void onKey(uint8_t code);
    void apply(ConfirmFlow::Input input);
    void render();
    void show(const char* headline, const char* detail, const char* hint);

    Model&      mModel;
    ConfirmFlow mFlow;

    lv_obj_t*   mRoot     = nullptr;
    lv_obj_t*   mInfo     = nullptr;
    lv_obj_t*   mHeadline = nullptr;
    lv_obj_t*   mCaption  = nullptr;
    lv_obj_t*   mDetail   = nullptr;
    lv_obj_t*   mHint     = nullptr;
    lv_obj_t*   mHold     = nullptr;
    lv_obj_t*   mNumber   = nullptr;
    lv_obj_t*   mHoldCrc  = nullptr;
    lv_timer_t* mTick     = nullptr;
    uint32_t    mShown    = 0;

    std::unique_ptr<SDK::LVGL::TimerRing> mRing;
    std::unique_ptr<SDK::LVGL::Buttons>   mButtons;
};
