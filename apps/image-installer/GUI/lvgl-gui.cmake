# The installer's GUI process, built on the SDK's LVGL port.
file(GLOB_RECURSE GUI_APP_SOURCES CONFIGURE_DEPENDS
    ${CMAKE_CURRENT_LIST_DIR}/gui/src/*.cpp
)

set(GUI_APP_INCLUDE_DIRS
    ${CMAKE_CURRENT_LIST_DIR}/gui/include
)
