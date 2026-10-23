#include <new>

#include "gui/MainScreen.hpp"
#include "gui/Model.hpp"

#include "SDK/GUI/LVGL/Draw.hpp"

namespace
{
alignas(Model)      uint8_t sModelStorage[sizeof(Model)];
alignas(MainScreen) uint8_t sScreenStorage[sizeof(MainScreen)];
}

extern "C" const lv_font_t* una_lvgl_default_font(void)
{
    return &lv_font_montserrat_14;
}

/// Called by the SDK's LVGL entry point once LVGL and the display are up.
extern "C" void una_lvgl_app_init(void)
{
    SDK::LVGL::Draw::init();
    Model*      model  = new (sModelStorage) Model();
    MainScreen* screen = new (sScreenStorage) MainScreen(*model);
    lv_screen_load(screen->root());
}
