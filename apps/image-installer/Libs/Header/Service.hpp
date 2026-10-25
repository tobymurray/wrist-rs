#pragma once

#include <cstddef>
#include <cstdint>

#include "SDK/Kernel/Kernel.hpp"

#include "Commands.hpp"

/// Checks `image.bin` once the GUI is up, and replaces the vendor kernel with it only on the
/// GUI's confirm naming that image's CRC-32.
class Service
{
public:
    explicit Service(SDK::Kernel& kernel);
    virtual ~Service() = default;

    void run();

private:
    static constexpr const char* kImageFile = "image.bin";
    static constexpr const char* kLogFile   = "install.log";
    /// Separate from `install.log`, which is never reopened.
    static constexpr const char* kStageLogFile = "stage.log";

    static constexpr size_t kPageSize = 8192;

    /// A service started with no GUI, by anything but the launcher, leaves after this
    /// without having touched flash.
    static constexpr uint32_t kNoGuiGraceMs = 5000;

    /// Added to wrist-flash's refusal codes; bank 1 is untouched.
    static constexpr int32_t kErrImageChanged = -14;
    static constexpr int32_t kErrImageUnreadable = -15;
    /// A confirm that arrived before a ready dry run, or that names another image.
    static constexpr int32_t kErrNotConfirmed = -16;

    /// `wrist_flash_unlock_selftest`: both unlocks returned.
    static constexpr int32_t kUnlockPassed = 3;
    /// `wrist_flash_erase_selftest`: erased, blank, programmed, read back, erased, blank.
    static constexpr int32_t kFlashTestPassed = 0x3F;

    SDK::Kernel&              mKernel;
    InstallerMessage::Verdict mVerdict{};
    bool                      mChecked   = false;
    bool                      mConfirmed = false;

    void report();
    void dryRun();
    void install(uint32_t confirmedCrc);

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

    int describeProtection(char* out, size_t capacity, int32_t eraseSelftest);

    int describeFlash(char* out, size_t capacity);

    /// 0 if `image.bin` is missing or empty.
    size_t   imageSize();
    bool     crcImage(size_t length, uint32_t& crc);
    /// 0, or a negative code; bank 1 is untouched either way.
    int32_t  stageImage(size_t length, uint32_t crc);
    bool     writeLog(const char* text, size_t length);
    bool     writeFile(const char* name, const char* text, size_t length);
};
