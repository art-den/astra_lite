#![allow(dead_code)]

use std::{fs::File, io::{BufReader, BufWriter}, path::Path};

use chrono::prelude::*;
use itertools::{izip, Itertools};

use super::{image::{Image, ImageLayer}, raw::{CalibrMethods, CfaType, FrameType, RawImage, RawImageInfo}, simple_fits::{FitsReader, SeekNRead}};

///////////////////////////////////////////////////////////////////////////////

// Raw image

pub fn load_raw_image_from_fits_stream(
    mut stream: impl SeekNRead
) -> anyhow::Result<RawImage> {
    let reader = FitsReader::new(&mut stream)?;
    let Some(image_hdu) = reader.headers.iter().find(|hdu| {
        hdu.dims().len() == 2
    }) else {
        anyhow::bail!("No RAW image found in fits data");
    };

    let bitdepth = image_hdu.get_i64("BITDEPTH").unwrap_or(image_hdu.bitpix() as i64) as i32;
    if bitdepth > 16 {
        anyhow::bail!("BITDEPTH > 16 ({}) is not supported", bitdepth);
    }
    if bitdepth < 0 {
        anyhow::bail!("FITS files with float values is not supported");
    }

    let width       = image_hdu.dims()[0];
    let height      = image_hdu.dims()[1];
    let exposure    = image_hdu.get_f64("EXPTIME").unwrap_or_default();
    let integr_time = image_hdu.get_f64("TOTALEXP");
    let bayer       = image_hdu.get_str("BAYERPAT").unwrap_or_default();
    let bin         = image_hdu.get_f64("XBINNING").unwrap_or(1.0) as u8;
    let gain        = image_hdu.get_f64("GAIN").unwrap_or(0.0) as i32;
    let offset      = image_hdu.get_f64("OFFSET").unwrap_or(0.0) as i32;
    let frame_str   = image_hdu.get_str("FRAME");
    let time_str    = image_hdu.get_str("DATE-OBS").unwrap_or_default();
    let camera      = image_hdu.get_str("INSTRUME").unwrap_or_default().to_string();
    let ccd_temp    = image_hdu.get_f64("CCD-TEMP");

    let max_value = ((1 << bitdepth) - 1) as u16;
    let cfa = CfaType::from_str(&bayer);
    let cfa_arr = cfa.get_array();
    let frame_type = FrameType::from_str(
        frame_str.as_deref().unwrap_or_default(),
        FrameType::Lights
    );

    let time =
        NaiveDateTime::parse_from_str(time_str, "%Y-%m-%dT%H:%M:%S%.3f")
            .map(|dt| Utc.from_utc_datetime(&dt))
            .ok();

    let info = RawImageInfo {
        time, width, height, gain, offset, cfa, bin,
        max_value, frame_type, exposure, integr_time,
        camera, ccd_temp,
        calibr_methods: CalibrMethods::empty(),
    };

    let data = FitsReader::read_data(&image_hdu, &mut stream)?;

    Ok(RawImage::new(info, data, cfa_arr))
}

pub fn load_raw_image_from_fits_file(file_name: &Path) -> anyhow::Result<RawImage> {
    let mut file = File::open(file_name)?;
    load_raw_image_from_fits_stream(&mut file)
}


///////////////////////////////////////////////////////////////////////////////

// Image layer

pub fn save_image_layer_to_tif_file(
    image_layer: &ImageLayer<u16>,
    file_name:   &Path
) -> anyhow::Result<()> {
    use tiff::encoder::*;
    let mut file = BufWriter::new(File::create(file_name)?);
    let mut decoder = TiffEncoder::new(&mut file)?;
    let mut tiff = decoder.new_image::<colortype::Gray16>(
        image_layer.width() as u32,
        image_layer.height() as u32
    )?;
    tiff.rows_per_strip(256)?;
    let mut pos = 0_usize;
    loop {
        let samples_count = tiff.next_strip_sample_count() as usize;
        if samples_count == 0 { break; }
        tiff.write_strip(&image_layer.as_slice()[pos..pos+samples_count])?;
        pos += samples_count;
    }
    tiff.finish()?;
    Ok(())

}

///////////////////////////////////////////////////////////////////////////////

// Simple image

pub fn load_image_from_tif_file(
    image:     &mut Image,
    file_name: &Path
) -> anyhow::Result<()> {
    use tiff::decoder::*;

    fn assign_img_data<S: Copy>(
        src:    &[S],
        img:    &mut Image,
        y1:     usize,
        y2:     usize,
        is_rgb: bool,
        cvt:    fn (from: S) -> u16
    ) -> anyhow::Result<()> {
        let from = y1 * img.width();
        let to = y2 * img.width();
        if is_rgb {
            let r_dst = &mut img.r.as_slice_mut()[from..to];
            let g_dst = &mut img.g.as_slice_mut()[from..to];
            let b_dst = &mut img.b.as_slice_mut()[from..to];
            for (dr, dg, db, (sr, sg, sb))
            in izip!(r_dst, g_dst, b_dst, src.iter().tuples()) {
                *dr = cvt(*sr);
                *dg = cvt(*sg);
                *db = cvt(*sb);
            }
        } else {
            let l_dst = &mut img.l.as_slice_mut()[from..to];
            for (d, s) in izip!(l_dst, src.iter()) {
                *d = cvt(*s);
            }
        }
        Ok(())
    }

    let file = BufReader::new(File::open(file_name)?);
    let mut decoder = Decoder::new(file)?;
    let (width, height) = decoder.dimensions()?;
    let is_rgb = match decoder.colortype()? {
        tiff::ColorType::Gray(_) => {
            image.make_monochrome(width as usize, height as usize, 0, u16::MAX);
            false
        }
        tiff::ColorType::RGB(_) => {
            image.make_color(width as usize, height as usize, 0, u16::MAX);
            true
        }
        ct =>
            anyhow::bail!("Color type {:?} unsupported", ct)
    };

    let chunk_size_y = decoder.chunk_dimensions().1 as usize;
    let chunks_cnt = decoder.strip_count()? as usize;
    let height = image.height();
    for chunk_index in 0..chunks_cnt {
        let chunk = decoder.read_chunk(chunk_index as u32)?;
        let y1 = (chunk_index * chunk_size_y) as usize;
        let y2 = (y1 + chunk_size_y).min(height);
        match chunk {
            DecodingResult::U8(data) =>
                assign_img_data(
                    &data,
                    image,
                    y1, y2,
                    is_rgb,
                    |v| v as u16 * 256
                ),

            DecodingResult::U16(data) =>
                assign_img_data(
                    &data,
                    image,
                    y1, y2,
                    is_rgb,
                    |v| v
                ),

            DecodingResult::F32(data) =>
                assign_img_data(
                    &data,
                    image,
                    y1, y2,
                    is_rgb,
                    |v| (v as f64 * u16::MAX as f64) as u16
                ),

            DecodingResult::F64(data) =>
                assign_img_data(
                    &data,
                    image,
                    y1, y2,
                    is_rgb,
                    |v| (v * u16::MAX as f64) as u16
                ),

            _ =>
                Err(anyhow::anyhow!("Format unsupported"))
        }?;
    }

    Ok(())
}

pub fn save_image_to_tif_file(image: &Image, file_name: &Path) -> anyhow::Result<()> {
    if image.is_monochrome() {
        save_image_layer_to_tif_file(&image.l, file_name)?;
    } else if image.is_color() {
        use tiff::encoder::*;
        let mut file = BufWriter::new(File::create(file_name)?);
        let mut decoder = TiffEncoder::new(&mut file)?;

        let mut tiff = decoder.new_image::<colortype::RGB16>(
            image.width() as u32,
            image.height() as u32
        )?;
        tiff.rows_per_strip(64)?;
        let mut strip_data = Vec::new();
        let mut pos = 0_usize;
        loop {
            let mut samples_count = tiff.next_strip_sample_count() as usize;
            if samples_count == 0 { break; }
            samples_count /= 3;
            strip_data.clear();
            let r_strip = &image.r.as_slice()[pos..pos+samples_count];
            let g_strip = &image.g.as_slice()[pos..pos+samples_count];
            let b_strip = &image.b.as_slice()[pos..pos+samples_count];
            for (r, g, b) in izip!(r_strip, g_strip, b_strip) {
                strip_data.push(*r);
                strip_data.push(*g);
                strip_data.push(*b);
            }
            tiff.write_strip(&strip_data)?;
            pos += samples_count;
        }
        tiff.finish()?;
    } else {
        panic!("Internal error");
    }
    Ok(())
}
