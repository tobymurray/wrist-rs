cmake_minimum_required(VERSION 3.21)
set(CMAKE_RUNTIME_OUTPUT_DIRECTORY "${CMAKE_CURRENT_SOURCE_DIR}/build")

set(APP_NAME "ImageInstaller")
set(APP_USER_NAME "Install")
set(APP_TYPE "Glance")
set(DEV_ID "UNA")
# sha256("una-rs#image-installer")[0:8]
set(APP_ID "854CD3EFBD7BA411")
set(APP_USE_ICONS Off)
set(RESOURCES_PATH "${CMAKE_CURRENT_SOURCE_DIR}/../Resources")
set(OUTPUT_PATH "${CMAKE_CURRENT_SOURCE_DIR}/../Output")
set(LIBS_PATH "${CMAKE_CURRENT_SOURCE_DIR}/../Libs")
set(RUST_WORKSPACE "${CMAKE_CURRENT_SOURCE_DIR}/../../..")

if("$ENV{UNA_SDK}" STREQUAL "")
    message(FATAL_ERROR "UNA_SDK is not set. Point it at an sdk-v1.3.0 checkout of the SDK.")
endif()

include($ENV{UNA_SDK}/cmake/una-app.cmake)
include($ENV{UNA_SDK}/cmake/una-sdk.cmake)
include(${LIBS_PATH}/libs.cmake)

project(${APP_NAME})

include(CheckCXXCompilerFlag)
check_cxx_compiler_flag(-fcyclomatic-complexity UNA_APP_HAS_CYCLOMATIC_COMPLEXITY)
if(NOT UNA_APP_HAS_CYCLOMATIC_COMPLEXITY)
    get_directory_property(_una_app_opts COMPILE_OPTIONS)
    list(REMOVE_ITEM _una_app_opts -fcyclomatic-complexity)
    set_directory_properties(PROPERTIES COMPILE_OPTIONS "${_una_app_opts}")
endif()

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

una_app_build_app()
