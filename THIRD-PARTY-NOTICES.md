# Third-party notices

fire-decode's own code is `MIT OR Apache-2.0` ([`LICENSE-MIT`](LICENSE-MIT),
[`LICENSE-APACHE`](LICENSE-APACHE)). The native C / C++ libraries below keep their own licences
(fire-decode plan, FD14). They are compiled or linked statically into every binary that enables
their feature, so they travel with that binary.

This file covers the native libraries only. The Rust crates fire-decode depends on are listed by
each app's own notices, generated from that app's `Cargo.lock`, because what lands in a binary
depends on the app's features and targets.

## Always linked

### libwebp - BSD-3-Clause

*WebP decoder, for lossy `.webp`. Version 1.6.0, compiled from source by the `libwebp-sys` crate.*
Copyright © 2010, Google Inc. All rights reserved. - <https://chromium.googlesource.com/webm/libwebp>
License text: [`licenses/BSD-3-Clause.txt`](licenses/BSD-3-Clause.txt). Google also grants a
patent licence for WebP; its terms are in the `PATENTS` file of the libwebp source.

## Feature `icc`

### Little-CMS (lcms2) - MIT

*ICC and nclx colour conversion. Version 2.19, compiled from source by the `lcms2-sys` crate.*
Copyright © 2023 Marti Maria Saguer - <https://littlecms.com>
License text: [`licenses/Little-CMS-MIT.txt`](licenses/Little-CMS-MIT.txt)

## Feature `psd`

### psd_sdk - BSD-2-Clause

*Photoshop `.psd` reader. Compiled from source by `crates/psd-sdk-sys`, from upstream commit
`f514495` with fire-decode's hardening patches (marked in the source; see
`crates/psd-sdk-sys/vendor/VENDOR.txt`).*
Copyright © 2011-2020 Molecular Matters GmbH - <https://github.com/MolecularMatters/psd_sdk>
License text: [`crates/psd-sdk-sys/vendor/LICENSE`](crates/psd-sdk-sys/vendor/LICENSE)

## Feature `heif`

Prebuilt static libraries in `crates/heif-sys/vendor/<target>/lib/`. Their versions and recipe are
in [`crates/heif-sys/vendor/VENDOR.txt`](crates/heif-sys/vendor/VENDOR.txt).

### libheif - LGPL-3.0-or-later

*HEIF / HEIC / AVIF container and API. Version 1.23.7 (macOS and Windows).*
Copyright © Dirk Farin and contributors - <https://github.com/strukturag/libheif>

### libde265 - LGPL-3.0-or-later

*HEVC decoder, for `.heic` / `.heif`. Version 1.1.3 (macOS and Windows).*
Copyright © struktur AG and contributors - <https://github.com/strukturag/libde265>

### dav1d - BSD-2-Clause

*AV1 decoder, for `.avif`. Version 1.5.4 (macOS and Windows).*
Copyright © 2018-2024 VideoLAN and dav1d authors. All rights reserved. -
<https://code.videolan.org/videolan/dav1d>
License text: [`licenses/BSD-2-Clause.txt`](licenses/BSD-2-Clause.txt)

> ### Your rights under the LGPL
>
> libheif and libde265 are licensed under the **GNU Lesser General Public License, version 3 or
> any later version** ([`licenses/LGPL-3.0.txt`](licenses/LGPL-3.0.txt), which incorporates
> [`licenses/GPL-3.0.txt`](licenses/GPL-3.0.txt)). A binary that enables `heif` links them
> statically, so LGPL-3.0 §4 entitles you to modify those libraries and relink the binary against
> your modified versions. fire-decode does not restrict that in any way:
>
> * **fire-decode's complete source is public**, and so is that of the apps that use it.
> * **The exact static libraries are in this repository's tree at each tag**, in
>   `crates/heif-sys/vendor/<target>/lib/`, beside the public headers they were built against.
> * **[`VENDOR.txt`](crates/heif-sys/vendor/VENDOR.txt) is the recipe**: the vcpkg baseline, the
>   triplets (committed in `crates/heif-sys/vendor/triplets/`), and the checks run after a build.
> * **Each tag's GitHub release carries the upstream source tarballs** of the exact versions. So
>   the corresponding source stays available from this repository, not only from upstream.
>
> **To relink:**
> 1. Build your own libheif / libde265 static libraries from modified sources.
> 2. Put them in a local checkout of the tag the app uses, in
>    `crates/heif-sys/vendor/<target>/lib/`, under the names already there.
> 3. Point the app at that checkout with a `[patch."https://github.com/psmyles/fire-decode"]`
>    entry in an untracked `.cargo/config.toml`, and build the app.
> 4. On macOS, re-sign the relinked app.
>
> Neither library is modified by fire-decode.
