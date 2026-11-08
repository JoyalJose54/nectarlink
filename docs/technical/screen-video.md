# Screen video implementation

Windows uses Media Foundation for screen encoding and video decoding. It checks
each adapter's decoder profile and enumerates screen encoders for that adapter's
LUID. Screen mirroring and the webcam share decoder selection; webcam textures
retain their producer's adapter LUID when passed to the virtual camera.

When decoding switches devices, the receiver requests a keyframe before
resuming.

Linux tries FFmpeg's NVENC, VAAPI and QSV hardware encoders, with OpenH264 as
the final H.264 fallback. It tries available CUDA devices and DRM render nodes
for hardware decoding before falling back to software. Decoded pictures are
downloaded as NV12 for the Qt renderer. Linux advertises `mirror.decode.hevc`
when its hardware HEVC decoder initializes.
