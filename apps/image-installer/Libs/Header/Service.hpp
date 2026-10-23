#pragma once

#include <cstddef>
#include <cstdint>

#include "SDK/Kernel/Kernel.hpp"
#include "SDK/Glance/GlanceControl.hpp"

/// Replaces the vendor kernel with `image.bin`, only when `ARM_INSTALL` holds that image's CRC-32.
class Service
{
public:
    explicit Service(SDK::Kernel& kernel);
    virtual ~Service() = default;

    void run();

private:
    static constexpr int16_t kTitleX = 18, kTitleY = 0, kTitleW = 205, kTitleH = 30;
    static constexpr int16_t kValueX = 18, kValueY = 28, kValueW = 205, kValueH = 40;

    static constexpr const char* kImageFile = "image.bin";
    static constexpr const char* kArmFile   = "ARM_INSTALL";
    static constexpr const char* kLogFile   = "install.log";
    /// Separate from `install.log`, which is never reopened.
    static constexpr const char* kStageLogFile = "stage.log";

    /// Only so a wrong file cannot be read into the stack.
    static constexpr size_t kArmFileMax = 32;

    static constexpr size_t kPageSize = 8192;

    /// Added to wrist-flash's refusal codes; bank 1 is untouched.
    static constexpr int32_t kErrImageChanged = -14;
    static constexpr int32_t kErrImageUnreadable = -15;

    enum class ArmState : uint8_t
    {
        Absent,
        Unparseable,
        Mismatch,
        Confirmed,
    };

    SDK::Kernel&             mKernel;
    SDK::Glance::Form        mGlanceUI;
    SDK::Glance::ControlText mGlanceTitle;
    SDK::Glance::ControlText mGlanceValue;
    bool                     mRan = false;

    bool configGui();
    void createGuiControls();
    void onGlanceTick();
    void install();

    /// Kept in step with crates/wrist-flash/src/hw.rs.
    static constexpr uint32_t kStatusAddress = 0x0805A000u;
    static constexpr uint32_t kStatusMagic   = 0x4641'4E55u;
    static constexpr uint32_t kKernelBase    = 0x08060000u;
    /// Kept in step with crates/wrist-flash/src/plan.rs (`STAGING_FIRST_PAGE`, `MAX_PAGES`).
    static constexpr unsigned kStagingFirstPage = 128u;
    static constexpr unsigned kStagingPages     = 68u;

    /// RM0456 Rev 7, 7.9, non-secure alias.
    static constexpr uint32_t kFlashBase = 0x40022000u;

    /// RM0456 Rev 7, Table 6, 13.4 and 11.8.30.
    static constexpr uint32_t kGpioBBase    = 0x42020400u;
    static constexpr uint32_t kGpioCBase    = 0x42020800u;
    static constexpr uint32_t kRccAhb2Enr1  = 0x46020C8Cu;
    int  describeDebugPath(char* out, size_t capacity);

    int describeProtection(char* out, size_t capacity);

    int describeFlash(char* out, size_t capacity);

    /// 0 if `image.bin` is missing or empty.
    size_t   imageSize();
    bool     crcImage(size_t length, uint32_t& crc);
    /// 0, or a negative code; bank 1 is untouched either way.
    int32_t  stageImage(size_t length, uint32_t crc);
    ArmState readArm(uint32_t expectedCrc, uint32_t& found);
    bool     writeLog(const char* text, size_t length);
    bool     writeFile(const char* name, const char* text, size_t length);
};
