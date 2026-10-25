cmake_minimum_required(VERSION 3.21)
set(CMAKE_RUNTIME_OUTPUT_DIRECTORY "${CMAKE_CURRENT_SOURCE_DIR}/build")

set(APP_NAME "ImageInstaller")
set(APP_USER_NAME "wrist-rs Setup")
set(APP_FILE_NAME "wrist-rs-installer")
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
    message(FATAL_ERROR "UNA_SDK is not set. Point it at an sdk-v1.6.1 checkout of the SDK.")
endif()

include($ENV{UNA_SDK}/cmake/una-app.cmake)
include($ENV{UNA_SDK}/cmake/una-sdk.cmake)
include(${LIBS_PATH}/libs.cmake)

project(${APP_NAME})

una_app_setup_version(BUILD_VERSION ${CMAKE_CURRENT_SOURCE_DIR})

set(RUST_TARGET "thumbv8m.main-none-eabihf")
set(RUST_LIB "${RUST_WORKSPACE}/target/${RUST_TARGET}/release/libwrist_flash.a")
set(RUST_GUI_LIB "${RUST_WORKSPACE}/target/${RUST_TARGET}/release/libwrist_installer_ui.a")

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

# A separate archive from wrist-flash's, so the GUI ELF carries no flash code.
add_custom_target(wrist_installer_ui_rust ALL
    COMMAND ${CMAKE_COMMAND} -E env --unset=MAKEFLAGS --unset=CARGO_MAKEFLAGS
            "RUSTFLAGS=${WRIST_RUSTFLAGS}"
            ${CARGO_EXECUTABLE} +${WRIST_NIGHTLY} rustc --locked -Z build-std=core --release -p wrist-installer-ui
            --target ${RUST_TARGET} --features device --crate-type staticlib
    WORKING_DIRECTORY ${RUST_WORKSPACE}
    BYPRODUCTS ${RUST_GUI_LIB}
    COMMENT "Building wrist-installer-ui -> libwrist_installer_ui.a"
)
# The two cargo builds share a target directory, so they take turns.
add_dependencies(wrist_installer_ui_rust wrist_flash_rust)

# The GUI shows the service's verdict and sends the confirm, through the SDK's CustomGUI entry
# point, which constructs the shell in Gui.hpp and runs it.
set(GUI_SOURCES
    $ENV{UNA_SDK}/Libs/Source/AppSystem/EntryPoint/CustomGUI/main.cpp
    ${GUI_PATH}/Gui.cpp
    ${UNA_SDK_SOURCES_COMMON}
)
set(GUI_INCLUDE_DIRS
    ${LIBS_INCLUDE_DIRS}
    ${UNA_SDK_INCLUDE_DIRS_COMMON}
    ${GUI_PATH}
)
# una_app_build_gui (una-app.cmake) links TOUCHGFX_LIBS, so the Rust screen goes in through it.
set(TOUCHGFX_LIBS ${RUST_GUI_LIB})
una_app_build_gui(${APP_NAME}GUI.elf)
add_dependencies(${APP_NAME}GUI.elf wrist_installer_ui_rust)

una_app_build_app()
