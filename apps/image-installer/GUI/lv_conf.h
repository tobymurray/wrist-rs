/*
 * The SDK's LVGL configuration, plus the fonts this screen draws in: Montserrat 28 for the
 * image's CRC, which the user compares with INSTALL-CRC.txt, 48 for the hold's countdown, and
 * 14 for the rest. All are compiled into LVGL, so the app carries no font files of its own.
 */
#ifndef IMAGE_INSTALLER_LV_CONF_H
#define IMAGE_INSTALLER_LV_CONF_H

#include "SDK/Port/LVGL/lv_conf.h"

#undef LV_FONT_MONTSERRAT_14
#define LV_FONT_MONTSERRAT_14 1
#undef LV_FONT_MONTSERRAT_28
#define LV_FONT_MONTSERRAT_28 1
#undef LV_FONT_MONTSERRAT_48
#define LV_FONT_MONTSERRAT_48 1

#endif
