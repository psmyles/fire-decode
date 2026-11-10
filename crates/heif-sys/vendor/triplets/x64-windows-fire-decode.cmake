# fire-decode's Windows HEIF stack (FD16, FD20): static libraries against the static CRT (/MT),
# release only, no plugin loading. Not yet run: the Windows re-vendor is still to do.
set(VCPKG_TARGET_ARCHITECTURE x64)
set(VCPKG_CRT_LINKAGE static)
set(VCPKG_LIBRARY_LINKAGE static)
set(VCPKG_BUILD_TYPE release)
if(PORT STREQUAL "libheif")
    # Plugin loading is a planted-library risk in a COM surrogate or the Quick Look sandbox.
    list(APPEND VCPKG_CMAKE_CONFIGURE_OPTIONS -DENABLE_PLUGIN_LOADING=OFF)
endif()
