#include "Gui.hpp"

#include "SDK/Kernel/KernelProviderGUI.hpp"
#include "SDK/Messages/CommandMessages.hpp"
#include "SDK/Messages/MessageGuard.hpp"
#include "SDK/Messages/MessageTypes.hpp"

#include "Commands.hpp"
#include "installer_ui.h"

#define LOG_MODULE_PRX   "Gui"
#define LOG_MODULE_LEVEL LOG_LEVEL_INFO
#include "SDK/UnaLogger/Logger.h"

extern "C" void wrist_installer_ui_panicked(const uint8_t* file, uint32_t len, uint32_t line)
{
    LOG_ERROR("Rust panic at %.*s:%lu\n", static_cast<int>(len), reinterpret_cast<const char*>(file),
              static_cast<unsigned long>(line));
    SDK::KernelProviderGUI::GetInstance().getKernel().sys.exit(1);
    while (true) {
    }
}

Gui::Gui(SDK::Kernel& kernel)
    : mKernel(kernel)
{
}

void Gui::queryDisplay()
{
    uint8_t depth = 0;
    auto*   cfg   = mKernel.comm.allocateMessage<SDK::Message::RequestDisplayConfig>();
    if (cfg != nullptr) {
        if (mKernel.comm.sendMessage(cfg, kResponseTimeoutMs) &&
            cfg->getResult() == SDK::MessageResult::SUCCESS) {
            mWidth  = cfg->width;
            mHeight = cfg->height;
            depth   = cfg->colorDepth;
        }
        mKernel.comm.releaseMessage(cfg);
    }
    // The screen is laid out for the 240x240 panel; anything else that fits the frame is
    // drawn as it is.
    if (mWidth <= 0 || mHeight <= 0 ||
        static_cast<uint32_t>(mWidth) * static_cast<uint32_t>(mHeight) > kPixels) {
        mWidth  = kSide;
        mHeight = kSide;
    }
    mUsable = depth <= kMaxBitsPerPixel;
    LOG_INFO("display %dx%d, %u bpp%s\n", mWidth, mHeight, depth, mUsable ? "" : ": not drawn");
}

void Gui::renderAndPush()
{
    if (!mResumed || !mUsable) {
        return;
    }
    wrist_installer_ui_render(mFrame, sizeof(mFrame), static_cast<uint16_t>(mWidth),
                              static_cast<uint16_t>(mHeight), mKernel.sys.getTimeMs());
    auto* upd = mKernel.comm.allocateMessage<SDK::Message::RequestDisplayUpdate>();
    if (upd != nullptr) {
        upd->pBuffer = mFrame;
        mKernel.comm.sendMessage(upd, kResponseTimeoutMs);
        mKernel.comm.releaseMessage(upd);
    }
}

bool Gui::act(uint32_t reply, uint32_t crc)
{
    if ((reply & WRIST_INSTALLER_UI_EXIT) != 0u) {
        LOG_INFO("exit\n");
        mKernel.sys.exit(0);
        return true;
    }
    if ((reply & WRIST_INSTALLER_UI_CONFIRM) != 0u) {
        LOG_INFO("confirm sent for %08lX\n", static_cast<unsigned long>(crc));
        SDK::send_msg<InstallerMessage::Confirm>(mKernel, crc);
    }
    if ((reply & WRIST_INSTALLER_UI_REDRAW) != 0u) {
        renderAndPush();
    }
    return false;
}

void Gui::run()
{
    if (wrist_installer_ui_abi() != WRIST_INSTALLER_UI_ABI) {
        LOG_ERROR("stale libwrist_installer_ui.a: ABI %lu, header %lu\n",
                  static_cast<unsigned long>(wrist_installer_ui_abi()),
                  static_cast<unsigned long>(WRIST_INSTALLER_UI_ABI));
        mKernel.sys.exit(1);
        return;
    }
    queryDisplay();

    while (true) {
        SDK::MessageBase* msg = nullptr;
        if (!mKernel.comm.getMessage(msg)) {
            continue;
        }

        uint32_t reply = 0;
        uint32_t crc   = 0;
        switch (msg->getType()) {
            case SDK::MessageType::COMMAND_APP_STOP:
                msg->setResult(SDK::MessageResult::SUCCESS);
                mKernel.comm.releaseMessage(msg);
                mKernel.sys.exit(0);
                return;

            case SDK::MessageType::COMMAND_APP_GUI_RESUME:
                mResumed = true;
                msg->setResult(SDK::MessageResult::SUCCESS);
                reply = WRIST_INSTALLER_UI_REDRAW;
                break;

            case SDK::MessageType::COMMAND_APP_GUI_SUSPEND:
                mResumed = false;
                msg->setResult(SDK::MessageResult::SUCCESS);
                // Button events stop while suspended, so a release would never arrive.
                wrist_installer_ui_input(WRIST_INSTALLER_UI_SUSPEND, mKernel.sys.getTimeMs(), nullptr);
                break;

            case SDK::MessageType::EVENT_GUI_TICK:
                msg->setResult(SDK::MessageResult::SUCCESS);
                reply = wrist_installer_ui_input(WRIST_INSTALLER_UI_TICK, mKernel.sys.getTimeMs(), &crc);
                break;

            case SDK::MessageType::EVENT_BUTTON: {
                using Id    = SDK::Message::EventButton::Id;
                using Event = SDK::Message::EventButton::Event;
                const auto* btn = static_cast<SDK::Message::EventButton*>(msg);
                msg->setResult(SDK::MessageResult::SUCCESS);
                // Off screen, a press is not meant for this app.
                if (!mResumed) {
                    break;
                }
                uint8_t input = 0xFF;
                if (btn->id == Id::SW2 && btn->event == Event::PRESS) {
                    input = WRIST_INSTALLER_UI_R1_PRESS;
                } else if (btn->id == Id::SW2 && btn->event == Event::RELEASE) {
                    input = WRIST_INSTALLER_UI_R1_RELEASE;
                } else if (btn->id == Id::SW4 && btn->event == Event::CLICK) {
                    input = WRIST_INSTALLER_UI_R2_CLICK;
                }
                if (input != 0xFF) {
                    reply = wrist_installer_ui_input(input, mKernel.sys.getTimeMs(), &crc);
                }
            } break;

            case InstallerMessage::REPORT: {
                const InstallerMessage::Verdict& v = static_cast<InstallerMessage::Report*>(msg)->verdict;
                reply = wrist_installer_ui_report(static_cast<uint8_t>(v.state), v.code, v.crc,
                                                  mKernel.sys.getTimeMs());
                msg->setResult(SDK::MessageResult::SUCCESS);
                mKernel.comm.sendResponse(msg);
            } break;

            default:
                msg->setResult(SDK::MessageResult::FAIL);
                mKernel.comm.sendResponse(msg);
                break;
        }
        mKernel.comm.releaseMessage(msg);

        if (act(reply, crc)) {
            return;
        }
    }
}
