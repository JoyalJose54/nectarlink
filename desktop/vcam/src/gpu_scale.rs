// SPDX-License-Identifier: GPL-3.0-or-later
//! NV12 scaling on the video processor, with a CPU fallback in the caller.

use windows::{
    Win32::{
        Foundation::{E_FAIL, RECT},
        Graphics::{Direct3D11::*, Dxgi::Common::*},
    },
    core::Interface,
};

pub(super) struct GpuScaler {
    device: ID3D11Device,
    input: (u32, u32),
    output: (u32, u32),
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
}
impl GpuScaler {
    pub(super) fn matches(&self, device: &ID3D11Device, input: (u32, u32), output: (u32, u32)) -> bool {
        self.device.as_raw() == device.as_raw() && self.input == input && self.output == output
    }
    #[allow(unsafe_code)]
    pub(super) fn new(
        device: &ID3D11Device,
        input: (u32, u32),
        output: (u32, u32),
    ) -> windows::core::Result<Self> {
        unsafe {
            let video: ID3D11VideoDevice = device.cast()?;
            let enumerator = video.CreateVideoProcessorEnumerator(&D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputFrameRate: DXGI_RATIONAL { Numerator: 30, Denominator: 1 },
                InputWidth: input.0,
                InputHeight: input.1,
                OutputFrameRate: DXGI_RATIONAL { Numerator: 30, Denominator: 1 },
                OutputWidth: output.0,
                OutputHeight: output.1,
                Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
            })?;
            let processor = video.CreateVideoProcessor(&enumerator, 0)?;
            Ok(Self { device: device.clone(), input, output, enumerator, processor })
        }
    }
    #[allow(unsafe_code)]
    pub(super) fn blit(
        &self,
        source: &ID3D11Texture2D,
        destination: &ID3D11Texture2D,
        index: u32,
        visible: (u32, u32),
    ) -> windows::core::Result<()> {
        unsafe {
            let video: ID3D11VideoDevice = self.device.cast()?;
            let context: ID3D11VideoContext = self.device.GetImmediateContext()?.cast()?;
            let mut input = None;
            video.CreateVideoProcessorInputView(
                source,
                &self.enumerator,
                &D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                    FourCC: 0,
                    ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                    Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                        Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 },
                    },
                },
                Some(&mut input),
            )?;
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            destination.GetDesc(&mut desc);
            let output_desc = if desc.ArraySize > 1 {
                D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                    ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2DARRAY,
                    Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                        Texture2DArray: D3D11_TEX2D_ARRAY_VPOV {
                            MipSlice: index % desc.MipLevels,
                            FirstArraySlice: index / desc.MipLevels,
                            ArraySize: 1,
                        },
                    },
                }
            } else {
                D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                    ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                    Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                        Texture2D: D3D11_TEX2D_VPOV { MipSlice: index },
                    },
                }
            };
            let mut output = None;
            video.CreateVideoProcessorOutputView(
                destination,
                &self.enumerator,
                &output_desc,
                Some(&mut output),
            )?;
            let (sw, sh) = visible;
            let (width, height) = self.output;
            let (fw, fh) = if width * sh <= height * sw {
                (width, ((sh * width / sw) & !1).max(2))
            } else {
                (((sw * height / sh) & !1).max(2), height)
            };
            let (x, y) = (((width - fw) / 2) & !1, ((height - fh) / 2) & !1);
            let source_rect = RECT { left: 0, top: 0, right: sw as i32, bottom: sh as i32 };
            let target =
                RECT { left: x as i32, top: y as i32, right: (x + fw) as i32, bottom: (y + fh) as i32 };
            let full = RECT { left: 0, top: 0, right: width as i32, bottom: height as i32 };
            context.VideoProcessorSetStreamSourceRect(&self.processor, 0, true, Some(&source_rect));
            context.VideoProcessorSetStreamDestRect(&self.processor, 0, true, Some(&target));
            context.VideoProcessorSetOutputTargetRect(&self.processor, true, Some(&full));
            context.VideoProcessorSetStreamAutoProcessingMode(&self.processor, 0, false);
            context.VideoProcessorSetOutputBackgroundColor(
                &self.processor,
                true,
                &D3D11_VIDEO_COLOR {
                    Anonymous: D3D11_VIDEO_COLOR_0 {
                        YCbCr: D3D11_VIDEO_COLOR_YCbCrA {
                            Y: 16.0 / 255.0,
                            Cb: 128.0 / 255.0,
                            Cr: 128.0 / 255.0,
                            A: 1.,
                        },
                    },
                },
            );
            let colors: ID3D11VideoContext1 = context.cast()?;
            colors.VideoProcessorSetStreamColorSpace1(
                &self.processor,
                0,
                DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
            );
            colors.VideoProcessorSetOutputColorSpace1(
                &self.processor,
                DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
            );
            let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                pInputSurface: std::mem::ManuallyDrop::new(input),
                ..Default::default()
            };
            let result = context.VideoProcessorBlt(
                &self.processor,
                output.as_ref().ok_or(E_FAIL)?,
                0,
                std::slice::from_ref(&stream),
            );
            drop(std::mem::ManuallyDrop::take(&mut stream.pInputSurface));
            result?;
            self.device.GetImmediateContext()?.Flush();
            Ok(())
        }
    }
}
