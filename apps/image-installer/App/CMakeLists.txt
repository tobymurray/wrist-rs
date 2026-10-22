cmake_minimum_required(VERSION 3.21)
set(CMAKE_RUNTIME_OUTPUT_DIRECTORY "${CMAKE_CURRENT_SOURCE_DIR}/build")

set(APP_NAME "ImageInstaller")
set(APP_USER_NAME "Install")
# A utility app is started from the launcher, so its service runs only when someone opens it,
# and its GUI asks for the confirm on screen.
set(APP_TYPE "Utility")
set(DEV_ID "UNA")
# sha256("una-rs#image-installer")[0:8]
set(APP_ID "854CD3EFBD7BA411")
set(RESOURCES_PATH "${CMAKE_CURRENT_SOURCE_DIR}/../Resources")
set(OUTPUT_PATH "${CMAKE_CURRENT_SOURCE_DIR}/../Output")
set(LIBS_PATH "${CMAKE_CURRENT_SOURCE_DIR}/../Libs")
set(GUI_PATH "${CMAKE_CURRENT_SOURCE_DIR}/../GUI")
set(RUST_WORKSPACE "${CMAKE_CURRENT_SOURCE_DIR}/../../..")

if("$ENV{UNA_SDK}" STREQUAL "")
    message(FATAL_ERROR "UNA_SDK is not set. Point it at an sdk-v1.6.1 checkout of the SDK, with ThirdParty/lvgl checked out.")
endif()

# The SDK's LVGL configuration, with the two fonts the screen uses.
set(UNA_LVGL_CONF "${GUI_PATH}/lv_conf.h")

include($ENV{UNA_SDK}/cmake/una-app.cmake)
include($ENV{UNA_SDK}/cmake/una-sdk.cmake)
include(${LIBS_PATH}/libs.cmake)
include(${GUI_PATH}/lvgl-gui.cmake)

project(${APP_NAME})

una_app_setup_version(BUILD_VERSION ${CMAKE_CURRENT_SOURCE_DIR})

set(RUST_TARGET "thumbv8m.main-none-eabihf")
set(RUST_LIB "${RUST_WORKSPACE}/target/${RUST_TARGET}/release/libwrist_flash.a")

find_program(CARGO_EXECUTABLE cargo REQUIRED)
set(WRIST_RUSTFLAGS "-C relocation-model=pic" CACHE STRING "Rust PIC and path flags")
set(WRIST_NIGHTLY "nightly-2026-10-19" CACHE STRING "PIC core compiler")

add_custom_target(wrist_flash_rust ALL
    COMMAND ${CMAKE_COMMAND} -E env --unset=MAKEFLAGS --unset=CARGO_MAKEFLAGS
            "RUSTFLAGS=${WRIST_RUSTFLAGS}"
            ${CARGO_EXECUTABLE} +${WRIST_NIGHTLY} rustc --locked -Z build-std=core --release -p wrist-flash
            --target ${RUST_TARGET} --features panic-handler --crate-type staticlib
    WORKING_DIRECTORY ${RUST_WORKSPACE}
    BYPRODUCTS ${RUST_LIB}
    COMMENT "Building wrist-flash -> libwrist_flash.a"
)

# The service does every check and every write to flash.
set(SERVICE_SOURCES
    ${LIBS_SOURCES}
    ${UNA_SDK_SOURCES_COMMON}
    ${UNA_SDK_SOURCES_SERVICE}
)
set(SERVICE_INCLUDE_DIRS
    ${LIBS_INCLUDE_DIRS}
    ${UNA_SDK_INCLUDE_DIRS_COMMON}
    ${UNA_SDK_INCLUDE_DIRS_SERVICE}
)
una_app_build_service(${APP_NAME}Service.elf)

target_link_libraries(${APP_NAME}Service.elf PRIVATE ${RUST_LIB})
add_dependencies(${APP_NAME}Service.elf wrist_flash_rust)

# The GUI shows the service's verdict and sends the confirm; it does not link wrist-flash.
set(GUI_SOURCES
    ${GUI_APP_SOURCES}
    ${UNA_SDK_SOURCES_COMMON}
    ${UNA_SDK_SOURCES_GUI_LVGL}
)
set(GUI_INCLUDE_DIRS
    ${LIBS_INCLUDE_DIRS}
    ${UNA_SDK_INCLUDE_DIRS_COMMON}
    ${UNA_SDK_INCLUDE_DIRS_GUI_LVGL}
    ${GUI_APP_INCLUDE_DIRS}
)
set(GUI_COMPILE_DEFINITIONS
    ${UNA_SDK_DEFINES_GUI_LVGL}
)
# LVGL's software renderer needs more stack than the SDK's 10 KB default, as in its LVGL tutorials.
set(GUI_STACK_SIZE "24*1024")
una_app_build_gui(${APP_NAME}GUI.elf)

una_app_build_app()
