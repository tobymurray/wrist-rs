#pragma once

#include <cstdint>

#include "SDK/Kernel/Kernel.hpp"

/// Carries the kernel's messages to crates/wrist-installer-ui and does what it answers.
class Gui
{
public:
    explicit Gui(SDK::Kernel& kernel);

    Gui(const Gui&)            = delete;
    Gui& operator=(const Gui&) = delete;

    void run();

private:
    static constexpr int16_t  kSide             = 240;
    static constexpr uint32_t kPixels           = 240u * 240u;
    static constexpr uint32_t kResponseTimeoutMs = 1000;
    /// A deeper panel would read past the one-byte-a-pixel frame, so none is pushed to it.
    static constexpr uint8_t kMaxBitsPerPixel = 8;

    void queryDisplay();
    /// Does what the screen answered; true when the app is leaving.
    bool act(uint32_t reply, uint32_t crc);
    void renderAndPush();

    SDK::Kernel& mKernel;
    int16_t      mWidth   = kSide;
    int16_t      mHeight  = kSide;
    bool         mUsable  = true;
    bool         mResumed = false;
    uint8_t      mFrame[kPixels]{};
};
