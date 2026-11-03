#pragma once

#include <cstdint>

#include "SDK/Messages/MessageBase.hpp"
#include "SDK/Messages/MessageTypes.hpp"

/// Messages between the installer's service and its GUI.
#pragma pack(push, 4)

namespace InstallerMessage
{

// Service to GUI.
constexpr SDK::MessageType::Type REPORT = 0x00000001;
// GUI to service.
constexpr SDK::MessageType::Type CONFIRM = 0x00000010;

/// What the service has found or is doing. Only `Ready` accepts a confirm.
enum class State : uint8_t
{
    Checking = 0,
    /// The firmware isolates apps; nothing was attempted.
    Isolated = 1,
    /// `image.bin` is missing, empty or unreadable.
    NoImage = 2,
    /// A check refused the image; `code` says which.
    Refused = 3,
    /// The flash unlock self-test failed; `code` is its result.
    UnlockFailed = 4,
    /// The scratch-page erase and program self-test failed; `code` is its result.
    FlashTestFailed = 5,
    /// The dry run passed for the image with CRC `crc`.
    Ready = 6,
    /// Confirmed: staging, then the commit, which restarts the watch.
    Installing = 7,
};

/// Plain data, so the GUI can keep the last one: a message is pool-allocated and not copyable.
struct Verdict
{
    State    state     = State::Checking;
    int32_t  code      = 0;
    uint32_t crc       = 0;
    uint32_t length    = 0;
    uint8_t  firstPage = 0;
    uint8_t  pages     = 0;
};

struct Report : public SDK::MessageBase
{
    Verdict verdict;

    Report()
        : SDK::MessageBase(REPORT)
        , verdict{}
    {
    }

    explicit Report(const Verdict& v)
        : Report()
    {
        verdict = v;
    }
};

/// Names the image by its CRC, so a confirm can only install the image the screen showed.
struct Confirm : public SDK::MessageBase
{
    uint32_t crc;

    Confirm()
        : SDK::MessageBase(CONFIRM)
        , crc(0)
    {
    }

    explicit Confirm(uint32_t imageCrc)
        : Confirm()
    {
        crc = imageCrc;
    }
};

}

#pragma pack(pop)
