// The gain-map HEIC test files, written by Apple's own encoder (ImageIO, macOS 15 or later), and
// ImageIO's own HDR decode of them as a reference. make.py builds and drives it:
//
//   gainmaps iso    <hdr.f32> <w> <h> <out.heic> <orientation> <headroom>
//   gainmaps apple  <sdr.u8>  <w> <h> <out.heic> <orientation> <headroom> <gain.u8> <gw> <gh>
//   gainmaps both   <hdr.f32> <w> <h> <out.heic> <orientation> <headroom> <gain.u8> <gw> <gh>
//   gainmaps decode <in.heic> <out.f32>
//
// .f32 files are linear Rec.709 RGBA floats (1.0 = SDR white); .u8 files are sRGB RGBA8 (sdr) or
// grey8 (gain). Images are given in coded orientation; <orientation> is the EXIF value ImageIO turns
// into irot. `decode` writes ImageIO's HDR rendering of the image, in coded orientation, as .f32,
// and prints its size and content headroom as JSON.

import CoreGraphics
import CoreVideo
import Foundation
import ImageIO
import UniformTypeIdentifiers

func fail(_ message: String) -> Never {
    FileHandle.standardError.write(Data((message + "\n").utf8))
    exit(1)
}

func contents(_ path: String) -> Data {
    guard let data = FileManager.default.contents(atPath: path) else { fail("cannot read \(path)") }
    return data
}

let floatInfo = CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedLast.rawValue
    | CGBitmapInfo.floatComponents.rawValue | CGBitmapInfo.byteOrder32Little.rawValue)
let linear = CGColorSpace(name: CGColorSpace.extendedLinearSRGB)!

/// The HDR picture, tagged with its content headroom: without it ImageIO's ISO encode treats the
/// picture as SDR and writes a flat gain map.
func floatImage(_ path: String, _ w: Int, _ h: Int, headroom: Float) -> CGImage {
    let provider = CGDataProvider(data: contents(path) as CFData)!
    let image = CGImage(width: w, height: h, bitsPerComponent: 32, bitsPerPixel: 128, bytesPerRow: w * 16,
                        space: linear, bitmapInfo: floatInfo, provider: provider, decode: nil,
                        shouldInterpolate: false, intent: .defaultIntent)!
    guard let tagged = CGImageCreateCopyWithContentHeadroom(headroom, image) else { fail("no headroom copy") }
    return tagged
}

func srgbImage(_ path: String, _ w: Int, _ h: Int) -> CGImage {
    let provider = CGDataProvider(data: contents(path) as CFData)!
    return CGImage(width: w, height: h, bitsPerComponent: 8, bitsPerPixel: 32, bytesPerRow: w * 4,
                   space: CGColorSpace(name: CGColorSpace.sRGB)!,
                   bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.noneSkipLast.rawValue),
                   provider: provider, decode: nil, shouldInterpolate: false, intent: .defaultIntent)!
}

/// Apple's HDR gain map: grey8 pixels, and the headroom in its XMP (no MakerNote).
func appleGainMap(_ path: String, _ gw: Int, _ gh: Int, _ headroom: Double) -> CFDictionary {
    let meta = CGImageMetadataCreateMutable()
    let ns = "http://ns.apple.com/HDRGainMap/1.0/" as CFString
    guard CGImageMetadataRegisterNamespaceForPrefix(meta, ns, "HDRGainMap" as CFString, nil) else {
        fail("cannot register the HDRGainMap namespace")
    }
    CGImageMetadataSetValueWithPath(meta, nil, "HDRGainMap:HDRGainMapVersion" as CFString, 131072 as CFNumber)
    CGImageMetadataSetValueWithPath(meta, nil, "HDRGainMap:HDRGainMapHeadroom" as CFString, headroom as CFNumber)
    let description: [CFString: Any] = [
        kCGImagePropertyWidth: gw, kCGImagePropertyHeight: gh, kCGImagePropertyBytesPerRow: gw,
        kCGImagePropertyPixelFormat: kCVPixelFormatType_OneComponent8,
    ]
    return [kCGImageAuxiliaryDataInfoData: contents(path),
            kCGImageAuxiliaryDataInfoDataDescription: description,
            kCGImageAuxiliaryDataInfoMetadata: meta] as CFDictionary
}

func write(_ out: String, _ image: CGImage, _ options: [CFString: Any], gainMap: CFDictionary?) {
    guard let dest = CGImageDestinationCreateWithURL(URL(fileURLWithPath: out) as CFURL,
                                                     UTType.heic.identifier as CFString, 1, nil) else {
        fail("cannot create \(out)")
    }
    CGImageDestinationAddImage(dest, image, options as CFDictionary)
    if let gainMap { CGImageDestinationAddAuxiliaryDataInfo(dest, kCGImageAuxiliaryDataTypeHDRGainMap, gainMap) }
    if !CGImageDestinationFinalize(dest) { fail("cannot write \(out)") }
}

func decode(_ path: String, _ out: String) {
    guard let source = CGImageSourceCreateWithURL(URL(fileURLWithPath: path) as CFURL, nil),
          let image = CGImageSourceCreateImageAtIndex(
              source, 0, [kCGImageSourceDecodeRequest: kCGImageSourceDecodeToHDR] as CFDictionary) else {
        fail("ImageIO cannot decode \(path)")
    }
    let (w, h) = (image.width, image.height)
    guard let ctx = CGContext(data: nil, width: w, height: h, bitsPerComponent: 32, bytesPerRow: w * 16,
                              space: linear, bitmapInfo: floatInfo.rawValue) else { fail("no context") }
    ctx.draw(image, in: CGRect(x: 0, y: 0, width: w, height: h))
    FileManager.default.createFile(atPath: out, contents: Data(bytes: ctx.data!, count: w * h * 16))
    print("{\"width\": \(w), \"height\": \(h), \"headroom\": \(image.contentHeadroom)}")
}

let a = CommandLine.arguments
guard a.count >= 3 else { fail("usage: see the top of gainmaps.swift") }
switch a[1] {
case "decode":
    decode(a[2], a[3])
case "iso", "apple", "both":
    let (w, h, out, orientation, headroom) = (Int(a[3])!, Int(a[4])!, a[5], Int(a[6])!, Double(a[7])!)
    var options: [CFString: Any] = [kCGImagePropertyOrientation: orientation,
                                    kCGImageDestinationLossyCompressionQuality: 0.9]
    var gainMap: CFDictionary?
    if a[1] != "iso" { gainMap = appleGainMap(a[8], Int(a[9])!, Int(a[10])!, headroom) }
    if a[1] != "apple" { options[kCGImageDestinationEncodeRequest] = kCGImageDestinationEncodeToISOGainmap }
    let image = a[1] == "apple" ? srgbImage(a[2], w, h) : floatImage(a[2], w, h, headroom: Float(headroom))
    write(out, image, options, gainMap: gainMap)
default:
    fail("unknown mode \(a[1])")
}
