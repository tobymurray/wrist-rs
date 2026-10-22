#include "gui/Model.hpp"

#include "SDK/Kernel/KernelProviderGUI.hpp"
#include "SDK/Messages/MessageGuard.hpp"
#include "SDK/Port/LVGL/LvglPort.hpp"

#define LOG_MODULE_PRX      "Model"
#define LOG_MODULE_LEVEL    LOG_LEVEL_INFO
#include "SDK/UnaLogger/Logger.h"

Model::Model()
    : mKernel(SDK::KernelProviderGUI::GetInstance().getKernel())
{
    SDK::LVGL::Port::GetInstance().setAppLifeCycleCallback(this);
    SDK::LVGL::Port::GetInstance().setCustomMessageHandler(this);
}

void Model::confirm(uint32_t crc)
{
    LOG_INFO("confirm sent for %08lX\n", static_cast<unsigned long>(crc));
    SDK::send_msg<InstallerMessage::Confirm>(mKernel, crc);
}

void Model::exitApp()
{
    SDK::LVGL::Port::GetInstance().setAppLifeCycleCallback(nullptr);
    SDK::LVGL::Port::GetInstance().setCustomMessageHandler(nullptr);
    mKernel.sys.exit();
}

uint32_t Model::nowMs() const
{
    return mKernel.sys.getTimeMs();
}

void Model::onStart()
{
}

void Model::onResume()
{
}

void Model::onSuspend()
{
    if (mListener != nullptr) {
        mListener->onSuspend();
    }
}

void Model::onStop()
{
}

bool Model::customMessageHandler(SDK::MessageBase* msg)
{
    if (msg->getType() == InstallerMessage::REPORT) {
        mVerdict = static_cast<InstallerMessage::Report*>(msg)->verdict;
        if (mListener != nullptr) {
            mListener->onVerdict(mVerdict);
        }
    }
    return true;
}
