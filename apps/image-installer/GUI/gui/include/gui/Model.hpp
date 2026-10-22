#pragma once

#include <cstdint>

#include "SDK/Interfaces/ICustomMessageHandler.hpp"
#include "SDK/Interfaces/IGuiLifeCycleCallback.hpp"
#include "SDK/Kernel/Kernel.hpp"

#include "Commands.hpp"

/// What the model tells the screen.
class ModelListener
{
public:
    virtual ~ModelListener() = default;

    virtual void onVerdict(const InstallerMessage::Verdict& verdict) = 0;
    /// The screen is no longer in front, so whatever was armed is disarmed.
    virtual void onSuspend() = 0;
};

/// The GUI's link to the kernel and to the installer's service.
class Model : public SDK::Interface::IGuiLifeCycleCallback,
              public SDK::Interface::ICustomMessageHandler
{
public:
    Model();

    void bind(ModelListener* listener) { mListener = listener; }

    /// Asks the service to install the image with this CRC.
    void confirm(uint32_t crc);

    void exitApp();

    const InstallerMessage::Verdict& verdict() const { return mVerdict; }

    uint32_t nowMs() const;

protected:
    void onStart() override;
    void onResume() override;
    void onSuspend() override;
    void onStop() override;

    bool customMessageHandler(SDK::MessageBase* msg) override;

private:
    ModelListener*            mListener = nullptr;
    const SDK::Kernel&        mKernel;
    InstallerMessage::Verdict mVerdict{};
};
