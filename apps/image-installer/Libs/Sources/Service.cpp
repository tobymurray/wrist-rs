#include "Service.hpp"

#include <cstddef>
#include <cstdint>
#include <cstdio>

#include "SDK/Messages/CommandMessages.hpp"
#include "SDK/Messages/MessageGuard.hpp"

#include "IsolationGate.hpp"

#define LOG_MODULE_PRX      "Service"
#define LOG_MODULE_LEVEL    LOG_LEVEL_INFO
#include "SDK/UnaLogger/Logger.h"

namespace
{

/// Mirrors `CPlan` in crates/wrist-flash.
struct CPlan
{
    uint8_t  firstPage;
    uint8_t  pages;
    uint8_t  stagingPage;
    uint32_t paddedLen;
    uint32_t quadwords;
};

extern "C" int32_t  wrist_flash_plan(uint32_t target, size_t len, CPlan* out);
extern "C" uint32_t wrist_flash_crc32_continue(uint32_t crc, const uint8_t* bytes, size_t len);
extern "C" int32_t  wrist_flash_stage_page(size_t len, uint32_t index, const uint8_t* chunk,
                                           size_t chunkLen);
extern "C" uint32_t wrist_flash_staged_crc(size_t len);
extern "C" uint32_t wrist_flash_staging_used();
extern "C" uint32_t wrist_flash_store_used();

extern "C" int32_t wrist_flash_install(uint32_t target, size_t len, uint32_t crc);

extern "C" int32_t wrist_flash_unlock_selftest();

extern "C" int32_t wrist_flash_erase_selftest();

alignas(4) uint8_t gChunk[8192];
char               gLog[3072];

}

Service::Service(SDK::Kernel& kernel)
    : mKernel(kernel)
{
}

void Service::run()
{
    LOG_INFO("Started\n");

    while (true) {
        SDK::MessageBase* msg;
        if (!mKernel.comm.getMessage(msg)) {
            continue;
        }

        switch (msg->getType()) {
            case SDK::MessageType::EVENT_GLANCE_START:
                if (configGui()) {
                    createGuiControls();
                    install();
                } else {
                    mKernel.comm.releaseMessage(msg);
                    mKernel.sys.exit(0);
                }
                break;

            case SDK::MessageType::COMMAND_APP_STOP:
            case SDK::MessageType::EVENT_GLANCE_STOP:
                mKernel.comm.releaseMessage(msg);
                return;

            case SDK::MessageType::EVENT_GLANCE_TICK:
                onGlanceTick();
                break;

            default:
                break;
        }

        mKernel.comm.releaseMessage(msg);
    }
}

void Service::install()
{
    if (mRan) {
        return;
    }
    mRan = true;

    const IsolationGate::Verdict verdict = IsolationGate::check();
    if (verdict != IsolationGate::Verdict::Allowed) {
        LOG_INFO("refused: firmware isolates apps (%u)\n", static_cast<unsigned>(verdict));
        mGlanceValue.setText("refused");
        return;
    }

    const size_t length = imageSize();
    if (length == 0) {
        LOG_INFO("no usable %s\n", kImageFile);
        mGlanceValue.setText("no image");
        return;
    }

    CPlan plan{};
    const int32_t planCode = wrist_flash_plan(kKernelBase, length, &plan);

    uint32_t crc = 0;
    if (planCode == 0 && !crcImage(length, crc)) {
        LOG_INFO("%s could not be read\n", kImageFile);
        mGlanceValue.setText("no image");
        return;
    }

    uint32_t       armCrc = 0;
    const ArmState arm    = readArm(crc, armCrc);

    const int32_t selftest = wrist_flash_unlock_selftest();

    const uint32_t stagingUsed = wrist_flash_staging_used();
    const uint32_t storeUsed   = wrist_flash_store_used();

    const char* armText = "no (dry run)";
    switch (arm) {
        case ArmState::Absent:      armText = "no (dry run)"; break;
        case ArmState::Unparseable: armText = "PRESENT BUT UNREADABLE"; break;
        case ArmState::Mismatch:    armText = "PRESENT BUT NAMES ANOTHER IMAGE"; break;
        case ArmState::Confirmed:   armText = "YES, crc confirmed"; break;
    }

    const int written = snprintf(
        gLog, sizeof(gLog),
        "una-rs image-installer\n"
        "image      : %s, %u bytes\n"
        "crc32      : %08lX\n"
        "target     : %08lX\n"
        "plan code  : %ld\n"
        "first page : %u (bank 1)\n"
        "pages      : %u\n"
        "padded len : %lu\n"
        "quadwords  : %lu\n"
        "staging    : bank 2 pages %u-%u, %lu of %u not blank now\n"
        "store      : bank 2 pages %u-%u, %lu of %u not blank\n"
        "%s       : %s\n"
        "arm names  : %08lX\n"
        "unlock test: %ld %s\n"
        "%s\n",
        kImageFile, static_cast<unsigned>(length),
        static_cast<unsigned long>(crc),
        static_cast<unsigned long>(kKernelBase),
        static_cast<long>(planCode),
        static_cast<unsigned>(plan.firstPage),
        static_cast<unsigned>(plan.pages),
        static_cast<unsigned long>(plan.paddedLen),
        static_cast<unsigned long>(plan.quadwords),
        static_cast<unsigned>(kStagingFirstPage),
        static_cast<unsigned>(kStagingFirstPage + kStagingPages - 1u),
        static_cast<unsigned long>(stagingUsed), static_cast<unsigned>(kStagingPages),
        static_cast<unsigned>(kStoreFirstPage),
        static_cast<unsigned>(kStoreFirstPage + kStorePages - 1u),
        static_cast<unsigned long>(storeUsed), static_cast<unsigned>(kStorePages),
        kArmFile, armText,
        static_cast<unsigned long>(armCrc),
        static_cast<long>(selftest),
        selftest == 3 ? "(unlock OK, re-unlock OK)" : "(FAILED -- will not commit)",
        planCode != 0                     ? "refused, nothing written"
        : arm == ArmState::Confirmed      ? "installing: staging in bank 2, then the vendor kernel is replaced"
        : arm == ArmState::Absent         ? "dry run only, nothing written"
                                          : "refused: arm file does not confirm this image");
    int total = (written > 0) ? written : 0;
    if (total > 0 && static_cast<size_t>(total) < sizeof(gLog)) {
        const int extra = describeFlash(gLog + total, sizeof(gLog) - static_cast<size_t>(total));
        if (extra > 0) {
            total += extra;
        }
        if (static_cast<size_t>(total) < sizeof(gLog)) {
            const int dbg = describeDebugPath(gLog + total, sizeof(gLog) - static_cast<size_t>(total));
            if (dbg > 0) {
                total += dbg;
            }
        }
        if (static_cast<size_t>(total) < sizeof(gLog)) {
            const int prot =
                describeProtection(gLog + total, sizeof(gLog) - static_cast<size_t>(total));
            if (prot > 0) {
                total += prot;
            }
        }
    }
    if (total > 0) {
        writeLog(gLog, static_cast<size_t>(total) < sizeof(gLog) ? static_cast<size_t>(total)
                                                                 : sizeof(gLog) - 1);
    }

    if (planCode != 0) {
        LOG_INFO("refused, plan code %ld\n", static_cast<long>(planCode));
        mGlanceValue.print("refused %ld", static_cast<long>(planCode));
        return;
    }

    if (arm == ArmState::Absent) {
        LOG_INFO("dry run: page %u, %u page(s), crc %08lX\n",
                 static_cast<unsigned>(plan.firstPage), static_cast<unsigned>(plan.pages),
                 static_cast<unsigned long>(crc));
        mGlanceValue.print("ready %08lX", static_cast<unsigned long>(crc));
        return;
    }

    if (arm != ArmState::Confirmed) {
        LOG_INFO("refused: %s %s (names %08lX, image is %08lX)\n", kArmFile, armText,
                 static_cast<unsigned long>(armCrc), static_cast<unsigned long>(crc));
        mGlanceValue.setText(arm == ArmState::Mismatch ? "arm mismatch" : "arm unreadable");
        return;
    }

    // Removed before committing: a success never returns, and an arm file that survived would install again.
    const bool consumed = mKernel.fs.remove(kArmFile);
    if (selftest != 3) {
        LOG_INFO("refusing to commit: unlock self-test returned %ld\n",
                 static_cast<long>(selftest));
        mGlanceValue.print("unlock %ld", static_cast<long>(selftest));
        return;
    }

    LOG_INFO("%s %s; staging %u bytes for %08lX\n", kArmFile,
             consumed ? "consumed" : "COULD NOT BE REMOVED", static_cast<unsigned>(length),
             static_cast<unsigned long>(kKernelBase));
    if (!consumed) {
        mGlanceValue.setText("arm stuck");
        return;
    }

    // Staged while the kernel's filesystem, which the commit erases, is still here.
    const int32_t  staged    = stageImage(length, crc);
    const uint32_t stagedCrc = (staged == 0) ? wrist_flash_staged_crc(length) : 0;
    int stageLen = snprintf(gLog, sizeof(gLog),
                            "una-rs image-installer, staging\n"
                            "image crc  : %08lX, %u bytes\n"
                            "staged     : code %ld, staged crc %08lX (%s)\n",
                            static_cast<unsigned long>(crc), static_cast<unsigned>(length),
                            static_cast<long>(staged), static_cast<unsigned long>(stagedCrc),
                            staged != 0        ? "staging failed, bank 1 untouched"
                            : stagedCrc == crc ? "matches, committing"
                                               : "MISMATCH, the commit will refuse");
    if (stageLen < 0) {
        stageLen = 0;
    } else if (static_cast<size_t>(stageLen) >= sizeof(gLog)) {
        stageLen = static_cast<int>(sizeof(gLog) - 1);
    }
    writeFile(kStageLogFile, gLog, static_cast<size_t>(stageLen));
    if (staged != 0) {
        LOG_INFO("staging refused, code %ld\n", static_cast<long>(staged));
        mGlanceValue.print("refused %ld", static_cast<long>(staged));
        return;
    }

    const int32_t code = wrist_flash_install(kKernelBase, length, crc);

    LOG_INFO("install refused, code %ld\n", static_cast<long>(code));
    const int again = snprintf(gLog + stageLen, sizeof(gLog) - static_cast<size_t>(stageLen),
                               "install refused before erasing anything in bank 1, code %ld\n",
                               static_cast<long>(code));
    if (again > 0 && static_cast<size_t>(stageLen + again) < sizeof(gLog)) {
        writeFile(kStageLogFile, gLog, static_cast<size_t>(stageLen + again));
    }
    mGlanceValue.print("refused %ld", static_cast<long>(code));
}

int Service::describeDebugPath(char* out, size_t capacity)
{
    // RM0456 Rev 7, 13.4.
    const volatile uint32_t* enr = reinterpret_cast<const volatile uint32_t*>(kRccAhb2Enr1);
    const uint32_t ahb2 = *enr;
    const bool bOn = (ahb2 & (1u << 1)) != 0u;
    const bool cOn = (ahb2 & (1u << 2)) != 0u;

    uint32_t bModer = 0, bPupdr = 0, bIdr = 0, bOdr = 0, cModer = 0, cAfrl = 0, cIdr = 0;
    if (bOn) {
        const volatile uint32_t* b = reinterpret_cast<const volatile uint32_t*>(kGpioBBase);
        bModer = b[0x00 / 4];
        bPupdr = b[0x0C / 4];
        bIdr   = b[0x10 / 4];
        bOdr   = b[0x14 / 4];
    }
    if (cOn) {
        const volatile uint32_t* c = reinterpret_cast<const volatile uint32_t*>(kGpioCBase);
        cModer = c[0x00 / 4];
        cAfrl  = c[0x20 / 4];
        cIdr   = c[0x10 / 4];
    }
    static const char* const kMode[4] = {"input", "output", "alternate", "analog"};
    const unsigned pb15Mode = (bModer >> 30) & 3u;
    const unsigned pb15Pull = (bPupdr >> 30) & 3u;
    const unsigned pb15Odr  = (bOdr >> 15) & 1u;
    const unsigned pb15Idr  = (bIdr >> 15) & 1u;
    const unsigned pc4Mode  = (cModer >> 8) & 3u;
    const unsigned pc4Af    = (cAfrl >> 16) & 0xFu;
    const unsigned pc4Idr   = (cIdr >> 4) & 1u;

    return snprintf(out, capacity,
                    "\n-- debug path (sheet 6: DEBUG_TX -> U22 SBU switch, SBUEN = DAM_DETECT = PB15) --\n"
                    "RCC_AHB2ENR1 %08lX  GPIOB clock %s, GPIOC clock %s\n"
                    "GPIOB MODER %08lX PUPDR %08lX ODR %08lX IDR %08lX\n"
                    "  PB15 DAM_DETECT : mode %s, pull %u, ODR %u, pin reads %u -> SBU switch %s\n"
                    "GPIOC MODER %08lX AFRL %08lX IDR %08lX\n"
                    "  PC4 DEBUG_TX    : mode %s, AF%u%s, pin reads %u\n",
                    static_cast<unsigned long>(ahb2), bOn ? "on" : "OFF", cOn ? "on" : "OFF",
                    static_cast<unsigned long>(bModer), static_cast<unsigned long>(bPupdr),
                    static_cast<unsigned long>(bOdr), static_cast<unsigned long>(bIdr),
                    bOn ? kMode[pb15Mode] : "(unclocked)", pb15Pull, pb15Odr, pb15Idr,
                    (bOn && pb15Idr) ? "ENABLED (DEBUG_TX reaches the connector)"
                                     : "OFF (nothing on PC4 reaches the connector)",
                    static_cast<unsigned long>(cModer), static_cast<unsigned long>(cAfrl),
                    static_cast<unsigned long>(cIdr), cOn ? kMode[pc4Mode] : "(unclocked)", pc4Af,
                    (pc4Mode == 2u && pc4Af == 7u) ? " (USART3_TX, as HARDWARE.md says)" : "",
                    pc4Idr);
}

int Service::describeProtection(char* out, size_t capacity)
{
    // Writes flash: a scratch page in bank 2 (crates/wrist-flash).
    const int32_t st_erase = wrist_flash_erase_selftest();

    const volatile uint32_t* f = reinterpret_cast<const volatile uint32_t*>(kFlashBase);
    const uint32_t nssr   = f[0x20 / 4];
    const uint32_t nscr   = f[0x28 / 4];
    const uint32_t eccr   = f[0x30 / 4];
    const uint32_t opsr   = f[0x34 / 4];
    const uint32_t optr   = f[0x40 / 4];
    const uint32_t wrp1a  = f[0x58 / 4];
    const uint32_t wrp1b  = f[0x5C / 4];
    const uint32_t wrp2a  = f[0x68 / 4];
    const uint32_t wrp2b  = f[0x6C / 4];

    const unsigned target = 48u;
    bool covered = false;
    for (uint32_t r : {wrp1a, wrp1b}) {
        const unsigned pstrt = r & 0xFFu;
        const unsigned pend  = (r >> 16) & 0xFFu;
        const bool     locked = ((r >> 31) & 1u) == 0u;
        if (locked && pstrt <= pend && target >= pstrt && target <= pend) {
            covered = true;
        }
    }

    return snprintf(out, capacity,
                    "\n-- flash protection --\n"
                    "NSSR  %08lX   NSCR  %08lX\n"
                    "ECCR  %08lX   OPSR  %08lX\n"
                    "OPTR  %08lX\n"
                    "WRP1AR %08lX  (pages %lu..%lu, %s)\n"
                    "WRP1BR %08lX  (pages %lu..%lu, %s)\n"
                    "WRP2AR %08lX   WRP2BR %08lX\n"
                    "  page 48 write-protected by a bank 1 area: %s\n"
                    "-- erase self-test on scratch page 34 (0x08244000) --\n"
                    "  raw %08lX -> erase %s, blank %s, program %s, readback %s,"
                    " re-erase %s, blank %s\n"
                    "  NSSR after erase %04lX : EOP %s%s\n",
                    static_cast<unsigned long>(nssr), static_cast<unsigned long>(nscr),
                    static_cast<unsigned long>(eccr), static_cast<unsigned long>(opsr),
                    static_cast<unsigned long>(optr),
                    static_cast<unsigned long>(wrp1a),
                    static_cast<unsigned long>(wrp1a & 0xFFu),
                    static_cast<unsigned long>((wrp1a >> 16) & 0xFFu),
                    ((wrp1a >> 31) & 1u) ? "unlocked" : "LOCKED",
                    static_cast<unsigned long>(wrp1b),
                    static_cast<unsigned long>(wrp1b & 0xFFu),
                    static_cast<unsigned long>((wrp1b >> 16) & 0xFFu),
                    ((wrp1b >> 31) & 1u) ? "unlocked" : "LOCKED",
                    static_cast<unsigned long>(wrp2a), static_cast<unsigned long>(wrp2b),
                    covered ? "YES" : "no",
                    static_cast<unsigned long>(st_erase),
                    (st_erase & 1) ? "ok" : "FAIL", (st_erase & 2) ? "yes" : "NO",
                    (st_erase & 4) ? "ok" : "FAIL", (st_erase & 8) ? "yes" : "NO",
                    (st_erase & 16) ? "ok" : "FAIL", (st_erase & 32) ? "yes" : "NO",
                    static_cast<unsigned long>((st_erase >> 8) & 0xFFFFu),
                    ((st_erase >> 8) & 1u) ? "SET"
                                           : "CLEAR (expected: EOP needs EOPIE=1, which we "
                                             "deliberately do not set)",
                    (((st_erase >> 8) & 0xFFFFu) == 0xFFFFu) ? " [poll timeout]" : "");
}

int Service::describeFlash(char* out, size_t capacity)
{
    const volatile uint32_t* st = reinterpret_cast<const volatile uint32_t*>(kStatusAddress);
    const volatile uint32_t* kn = reinterpret_cast<const volatile uint32_t*>(kKernelBase);

    const bool began    = (st[0] == kStatusMagic) && (((st[1] >> 16) & 0xFFFFu) == 9u);
    const bool finished = (st[4] == kStatusMagic);
    const uint32_t outcome = (st[5] >> 16) & 0xFFFFu;

    // Kept in step with the OUTCOME_* codes in crates/wrist-flash/src/hw.rs.
    const char* verdict = "blank: the installer never got as far as unlocking flash";
    if (began && !finished) {
        verdict = "BEGAN but no outcome: the kernel write started and did not finish";
    } else if (finished) {
        switch (outcome) {
            case 1:  verdict = "OUTCOME 1: erase+program+verify all SUCCEEDED"; break;
            case 2:  verdict = "OUTCOME 2: ERASE failed (detail = page index << 24 | NSSR errors)"; break;
            case 3:  verdict = "OUTCOME 3: PROGRAM failed (detail = NSSR errors)"; break;
            case 4:  verdict = "OUTCOME 4: VERIFY failed (detail = byte offset)"; break;
            case 5:  verdict = "OUTCOME 5: erase reported OK but the page was NOT BLANK "
                               "(detail = the word read)"; break;
            case 6:  verdict = "OUTCOME 6: a flash status poll TIMED OUT (detail = FFFF, "
                               "with the page index << 24 when erasing)"; break;
            case 7:  verdict = "OUTCOME 7: the erase command DID NOT STICK -- NSCR read back "
                               "locked or without PER (detail = page index << 24 | E000 | NSCR >> 16)"; break;
            default: verdict = "outcome record present, code unrecognised"; break;
        }
    } else if (st[0] != 0xFFFFFFFFu) {
        verdict = "something else is written at the status page";
    }

    const char* statusState = st[0] == 0xFFFFFFFFu  ? "blank"
                              : st[0] == kStatusMagic ? "ours"
                                                      : "FOREIGN DATA (install will refuse, -8)";

    // RM0456 Rev 7, 7.9.7.
    char bits[96] = "";
    if (finished && (outcome == 2u || outcome == 3u)) {
        const uint32_t e = st[6] & 0xFFFFu;
        snprintf(bits, sizeof(bits), "  nssr errors: %s%s%s%s%s%s%s\n",
                 (e & (1u << 1)) ? "OPERR " : "", (e & (1u << 3)) ? "PROGERR " : "",
                 (e & (1u << 4)) ? "WRPERR " : "", (e & (1u << 5)) ? "PGAERR " : "",
                 (e & (1u << 6)) ? "SIZERR " : "", (e & (1u << 7)) ? "PGSERR " : "",
                 (e == 0xFFFFu) ? "(poll timeout)" : (e == 0u ? "(none set)" : ""));
    }

    return snprintf(out, capacity,
                    "\n-- flash forensics --\n"
                    "status @ %08lX : %08lX %08lX %08lX %08lX\n"
                    "                  %08lX %08lX %08lX %08lX\n"
                    "  began      : %s\n"
                    "  finished   : %s\n"
                    "  first page : %lu, pages %lu\n"
                    "  detail     : %08lX\n"
                    "  image crc  : %08lX\n"
                    "%s"
                    "  %s\n"
                    "witness page (bank 1, 45): %s\n"
                    "kernel @ %08lX : SP %08lX  reset %08lX\n",
                    static_cast<unsigned long>(kStatusAddress),
                    static_cast<unsigned long>(st[0]), static_cast<unsigned long>(st[1]),
                    static_cast<unsigned long>(st[2]), static_cast<unsigned long>(st[3]),
                    static_cast<unsigned long>(st[4]), static_cast<unsigned long>(st[5]),
                    static_cast<unsigned long>(st[6]), static_cast<unsigned long>(st[7]),
                    began ? "yes" : "no", finished ? "yes" : "no",
                    static_cast<unsigned long>((st[5] >> 8) & 0xFFu),
                    static_cast<unsigned long>(st[5] & 0xFFu),
                    static_cast<unsigned long>(st[6]),
                    static_cast<unsigned long>(st[7]),
                    bits,
                    verdict,
                    statusState,
                    static_cast<unsigned long>(kKernelBase),
                    static_cast<unsigned long>(kn[0]), static_cast<unsigned long>(kn[1]));
}

Service::ArmState Service::readArm(uint32_t expectedCrc, uint32_t& found)
{
    auto f = mKernel.fs.file(kArmFile);
    if (!f || !f->exist()) {
        return ArmState::Absent;
    }

    // Anything but exactly one 8-digit CRC refuses, so a typo or a truncated write never installs.
    const size_t size = f->size();
    if (size == 0 || size > kArmFileMax) {
        return ArmState::Unparseable;
    }
    if (!f->open(false, false)) {
        return ArmState::Unparseable;
    }
    char   buffer[kArmFileMax + 1] = {};
    size_t read                    = 0;
    const bool ok                  = f->read(buffer, size, read);
    f->close();
    if (!ok || read == 0) {
        return ArmState::Unparseable;
    }
    buffer[read] = '\0';

    const char* p = buffer;
    while (*p == ' ' || *p == '\t' || *p == '\r' || *p == '\n') {
        ++p;
    }
    if (p[0] == '0' && (p[1] == 'x' || p[1] == 'X')) {
        p += 2;
    }

    uint32_t value  = 0;
    unsigned digits = 0;
    for (; *p != '\0'; ++p) {
        uint32_t nibble;
        if (*p >= '0' && *p <= '9') {
            nibble = static_cast<uint32_t>(*p - '0');
        } else if (*p >= 'a' && *p <= 'f') {
            nibble = static_cast<uint32_t>(*p - 'a') + 10u;
        } else if (*p >= 'A' && *p <= 'F') {
            nibble = static_cast<uint32_t>(*p - 'A') + 10u;
        } else {
            break;
        }
        if (digits == 8u) {
            return ArmState::Unparseable;
        }
        value = (value << 4) | nibble;
        ++digits;
    }
    if (digits != 8u) {
        return ArmState::Unparseable;
    }

    found = value;
    return (value == expectedCrc) ? ArmState::Confirmed : ArmState::Mismatch;
}

size_t Service::imageSize()
{
    auto f = mKernel.fs.file(kImageFile);
    if (!f || !f->exist()) {
        return 0;
    }
    return f->size();
}

bool Service::crcImage(size_t length, uint32_t& crc)
{
    auto f = mKernel.fs.file(kImageFile);
    if (!f || !f->open(false, false)) {
        return false;
    }
    uint32_t running = 0xFFFFFFFFu;
    size_t   done    = 0;
    bool     ok      = true;
    while (ok && done < length) {
        const size_t want = (length - done < kPageSize) ? length - done : kPageSize;
        size_t       got  = 0;
        ok = f->read(reinterpret_cast<char*>(gChunk), want, got) && got == want;
        if (ok) {
            running = wrist_flash_crc32_continue(running, gChunk, got);
            done += got;
        }
    }
    f->close();
    crc = ~running;
    return ok;
}

int32_t Service::stageImage(size_t length, uint32_t crc)
{
    auto f = mKernel.fs.file(kImageFile);
    if (!f || !f->open(false, false)) {
        return kErrImageUnreadable;
    }
    uint32_t running = 0xFFFFFFFFu;
    size_t   done    = 0;
    uint32_t index   = 0;
    int32_t  code    = 0;
    while (code == 0 && done < length) {
        const size_t want = (length - done < kPageSize) ? length - done : kPageSize;
        size_t       got  = 0;
        if (!f->read(reinterpret_cast<char*>(gChunk), want, got) || got != want) {
            code = kErrImageUnreadable;
            break;
        }
        running = wrist_flash_crc32_continue(running, gChunk, got);
        code = wrist_flash_stage_page(length, index, gChunk, got);
        done += got;
        ++index;
    }
    f->close();
    if (code != 0) {
        LOG_INFO("staging stopped at page %lu, code %ld\n", static_cast<unsigned long>(index),
                 static_cast<long>(code));
        return code;
    }
    LOG_INFO("staged %lu page(s)\n", static_cast<unsigned long>(index));
    return (~running == crc) ? 0 : kErrImageChanged;
}

bool Service::writeLog(const char* text, size_t length)
{
    return writeFile(kLogFile, text, length);
}

bool Service::writeFile(const char* name, const char* text, size_t length)
{
    auto f = mKernel.fs.file(name);
    if (!f || !f->open(true, true)) {
        return false;
    }
    size_t written = 0;
    const bool ok = f->write(text, length, written);
    f->flush();
    f->close();
    return ok && written == length;
}

void Service::onGlanceTick()
{
    if (mGlanceUI.isInvalid()) {
        if (auto upd = SDK::make_msg<SDK::Message::RequestGlanceUpdate>(mKernel)) {
            upd->name           = APP_NAME;
            upd->controls       = mGlanceUI.data();
            upd->controlsNumber = static_cast<uint32_t>(mGlanceUI.size());
            upd.send(100);
        }
        mGlanceUI.setValid();
    }
}

bool Service::configGui()
{
    if (auto gc = SDK::make_msg<SDK::Message::RequestGlanceConfig>(mKernel)) {
        if (gc.send(100) && gc.ok() && gc->maxControls >= 2) {
            mGlanceUI.setWidth(gc->width);
            mGlanceUI.setHeight(gc->height);
            return true;
        }
    }
    return false;
}

void Service::createGuiControls()
{
    mGlanceTitle = mGlanceUI.createText();
    mGlanceTitle.pos({kTitleX, kTitleY}, {kTitleW, kTitleH})
        .font(GlanceFont_t::GLANCE_FONT_POPPINS_SEMIBOLD_20)
        .color(GlanceColor_t::GLANCE_COLOR_TEAL)
        .setText("Install")
        .alignment(GlanceAlignH_t::GLANCE_ALIGN_H_CENTER);

    mGlanceValue = mGlanceUI.createText();
    // The largest Poppins SemiBold in which `ready` and eight hex digits fit kValueW.
    mGlanceValue.pos({kValueX, kValueY}, {kValueW, kValueH})
        .font(GlanceFont_t::GLANCE_FONT_POPPINS_SEMIBOLD_20)
        .color(GlanceColor_t::GLANCE_COLOR_WHITE)
        .setText("...")
        .alignment(GlanceAlignH_t::GLANCE_ALIGN_H_CENTER);
}
