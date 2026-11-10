# fire-decode's macOS HEIF stack (FD16): static, release only, for macOS 11.0, no plugin loading.
set(VCPKG_TARGET_ARCHITECTURE arm64)
set(VCPKG_CRT_LINKAGE dynamic)
set(VCPKG_LIBRARY_LINKAGE static)
set(VCPKG_CMAKE_SYSTEM_NAME Darwin)
set(VCPKG_OSX_ARCHITECTURES arm64)
set(VCPKG_OSX_DEPLOYMENT_TARGET 11.0)
set(VCPKG_BUILD_TYPE release)
if(PORT STREQUAL "libheif")
    # Plugin loading is a planted-library risk in a COM surrogate or the Quick Look sandbox.
    list(APPEND VCPKG_CMAKE_CONFIGURE_OPTIONS -DENABLE_PLUGIN_LOADING=OFF)
endif()
