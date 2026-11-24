#!/usr/bin/env bash
# Fetch the KTX / KTX2 test files from KTX-Software's own test resources, at one pinned commit,
# and check each against its SHA-256 (the Git LFS object id KTX-Software records for it). The
# files are committed; this is how they were got, and how to get them again.
#
#   fixtures/ktx/fetch.sh            fetch any that are missing, then check them all
#   fixtures/ktx/fetch.sh --check    check only
#
# They are Git LFS objects upstream, so they come from media.githubusercontent.com, not
# raw.githubusercontent.com (which serves the ~130-byte LFS pointers). Licences: README.md.
set -euo pipefail

commit=983695c105618462b8e96458b378222e99faec98
base="https://media.githubusercontent.com/media/KhronosGroup/KTX-Software/$commit/tests/resources"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
check_only=0
[ "${1:-}" = "--check" ] && check_only=1

sha256() {
    if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1
    else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

# sha256, then the path under tests/resources/ (which is where it lands here, input/png/ aside).
files=(
    "726393af40730a2c349b6a70217c6da89d4bc6fee78bac30935088f62a2b5581 ktx2/color_grid_blze.ktx2"
    "5e95d3500b1dc9817ba184ff6856d0c8bc3a6d9e20caf803edcbde82a32199e0 ktx2/color_grid_uastc_zstd_5.ktx2"
    "be303d6d69895e2080776453b28a0d54b8c20c7798759351c9543d1555a10a87 ktx2/color_grid_zstd_5.ktx2"
    "e24340bf5a06666bbb112c62bd9c676e979f8a0f6f8833b4efa667285a48b07e ktx2/r8g8b8a8_srgb_mip.ktx2"
    "6faf1fb53718c81eb8eef1f73320c864c7bfb7af14b9748225458422e26f08c8 ktx2/r8g8b8a8_srgb_mip_etc2.ktx2"
    "015d6d645de2afa02542a0bdc1b52ec8e39170c514eb5c8e21368408b84faad9 ktx2/r8g8b8a8_srgb_mip_astc.ktx2"
    "0ebd1ca3c9c8016c97226c7144606a78c900ddb42819495aea4acd05c76d11ff ktx2/r8g8b8a8_srgb_mip_blze.ktx2"
    "b9f7b8719fa16f53dc5706438d8e04b0b0af21e756492579849d9c5809ea06d3 ktx2/r8g8b8a8_srgb_array_7_mip.ktx2"
    "469833ae8e3c22bcfdf6c21473b1b06df77b3d09eac4f7f7d60c59e8d6056507 ktx2/r8g8b8a8_srgb_3d_7.ktx2"
    "5b442fbbbc8ba46a330e644d594054e84971f93e1eb6728cbfff6d523809f170 ktx2/etc2_unorm_array_7.ktx2"
    "7658aaa628405ba7744b57ba093d6cc6bd42e77f85bfc5ec2ddcd3f37041d1d5 ktx2/astc_8x8_unorm_array_7.ktx2"
    "161e4a31ccf8a38edf0125ddfe9ab67e94f56a148ca5b63c9924707363b51ffe ktx2/bc3_unorm_array_7.ktx2"
    "08a3ac4af02412a75a7d0fbf1e0ba0d128f14aaa2b4e9ab916417ea382030202 ktx2/orient_up_metadata.ktx2"
    "7bcac6605de7a372416f95164b0757c89901b18046f658feeb8322c2a3e1edd4 ktx2/orient_down_metadata.ktx2"
    "df777e1ab9371ca5650d3d397b9d3a7f23b806182a904afd86acf39619e0aa3f ktx2/alpha_complex_premultiplied.ktx2"
    "8996e7d69edbb863d02642f22621ca676d960184a74b8becfb816cfa8d9f5ad3 ktx2/alpha_complex_straight.ktx2"
    "723a272653cd2e7931a63a445110dcf17a4c059687858f245228c8ca4c9e9209 ktx2/Desk_uastc_hdr4x4_zstd_15.ktx2"
    "48a8b5441e0018a39590116de834b226a2ec4de29517b097880390f9f4141a34 ktx/etc1.ktx"
    "9c6133cde76286b9a0a4de4102037e2121c0984d6ac4b10e8be2ef5f96a18203 ktx/etc2_rgb.ktx"
    "0f835ddb08aa6ddc682dbf7685fdfeea6693c3f6adcbabfbec95ec83062a43ef ktx/etc2_rgba1.ktx"
    "ad5c697098f8e5b452c87b6d4cb1fbdb51399b4f52c1b1e3e74790c820b40db3 ktx/etc2_rgba8.ktx"
    "f74e8716e5f6b49d321cc2cefe7f7b60362affd585f501134676a843afea34b2 ktx/conftestimage_R11_EAC.ktx"
    "5dd368207e358607b7596f5a73201934d33f87eb2dec71cdc16ac1d20eaf1e1d ktx/conftestimage_RG11_EAC.ktx"
    "b4369caeb34d25a9a9fa01de3f47d82769be2e7703c7a09178c9ade99c429a2c ktx/conftestimage_SIGNED_R11_EAC.ktx"
    "43624d9d106d4eb4bb5943bcd7d11470571590300078754c7288074436e05789 ktx/conftestimage_SIGNED_RG11_EAC.ktx"
    "da0fe3fcd31da0a99682972dee68079a3e0dfb6d8092fefbb3cacc6e92a6e024 ktx/r8g8b8_srgb.ktx"
    "144a9af1beb84939c74424062ee1ce53fcd64e6c36b32e71ffa79e2722dd84dd ktx/r8g8b8_srgb_mip.ktx"
    "f99f7d4a88bd2eb3be9b5841db55dfd57b3cdceae91ea1f90a286919ccc0425c ktx/not4_r8g8b8_srgb.ktx"
    "da74ead1bac6259a5110e8cbcd85610852780cc7b6b6c2c2ae72061c2a28d71f ktx/orient_up_metadata.ktx"
    "0ab5d8e17e22c51ad70ddabe25387647b0d81c2b1f316942e44320826d849b00 ktx/orient_down_metadata.ktx"
    "76532fa0853853c2dced2c5e6c6eabf8c07d20cb2956cc0fb94046ca4e9101ca ktx/l8_unorm_metadata.ktx"
    "94c364ad7f7f6c8c4bccfffb48d74a1e5bd9cb6934ae46130a5cc76cdcb6cc57 ktx/bc3_unorm_array_7.ktx"
    "8305e9adea781bc9c2eae6c43f7a9ee76dff4ad5fb9ba689be0d8e1fd8ef4d35 ktx/etc2_unorm_array_7.ktx"
    "046f6f04070340bfaa465129a49e4ea082923a5de83fc9d819d6abea4bf779cf ktx/astc_8x8_unorm_array_7.ktx"
    "04065469c3fc0fb704797d49a8c4e1753efe547c3749702b648ab78e4b7a1953 input/png/color_grid.png"
)

bad=0
for entry in "${files[@]}"; do
    want=${entry%% *}
    src=${entry#* }
    dst="$here/${src#input/png/}"
    if [ ! -f "$dst" ] && [ "$check_only" = 0 ]; then
        mkdir -p "$(dirname "$dst")"
        curl -fsSL -o "$dst" "$base/$src"
    fi
    if [ ! -f "$dst" ]; then
        echo "missing: ${dst#"$here"/}"; bad=1
    elif [ "$(sha256 "$dst")" != "$want" ]; then
        echo "checksum mismatch: ${dst#"$here"/}"; bad=1
    fi
done
[ "$bad" = 0 ] && echo "fixtures/ktx: ${#files[@]} files, all present and matching."
exit "$bad"
