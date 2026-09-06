use crate::erofs_fs::*;
use crate::error::{Error, Result};
use crate::sb::SbInfo;

fn z_erofs_fixup_insize(src: &[u8]) -> usize {
    src.iter().take_while(|&&b| b == 0).count()
}

fn z_erofs_decompress_lz4(
    src: &[u8],
    dst: &mut [u8],
    decodedlength: u64,
    decodedskip: u64,
    partial_decoding: bool,
) -> Result<()> {
    let inputmargin = z_erofs_fixup_insize(src);
    if inputmargin >= src.len() {
        return Err(Error::efscorrupted());
    }

    if decodedskip != 0 {
        let mut buff = vec![0u8; decodedlength as usize];
        let ret = do_lz4(src, &mut buff, inputmargin, decodedlength, partial_decoding)?;
        if ret != decodedlength as usize {
            return Err(Error::eio());
        }
        dst.copy_from_slice(&buff[decodedskip as usize..]);
        return Ok(());
    }
    let ret = do_lz4(src, dst, inputmargin, decodedlength, partial_decoding)?;
    if ret != decodedlength as usize {
        return Err(Error::eio());
    }
    Ok(())
}

fn do_lz4(
    src: &[u8],
    dst: &mut [u8],
    inputmargin: usize,
    decodedlength: u64,
    partial: bool,
) -> Result<usize> {
    // lz4-sys 1.11 does not export this symbol in its bindings, but the
    // bundled liblz4 provides it and is linked statically.
    unsafe extern "C" {
        fn LZ4_decompress_safe_partial(
            source: *const i8,
            dest: *mut i8,
            sourceSize: i32,
            targetOutputSize: i32,
            maxOutputSize: i32,
        ) -> i32;
    }

    let inlen = (src.len() - inputmargin) as i32;
    let outlen = decodedlength as i32;
    // SAFETY: buffers are valid for the whole call; lz4 reads at most inlen
    // bytes from src and writes at most outlen bytes to dst.
    let ret = unsafe {
        if partial {
            LZ4_decompress_safe_partial(
                src[inputmargin..].as_ptr() as *const i8,
                dst.as_mut_ptr() as *mut i8,
                inlen,
                outlen,
                outlen,
            )
        } else {
            lz4_sys::LZ4_decompress_safe(
                src[inputmargin..].as_ptr() as *const i8,
                dst.as_mut_ptr() as *mut i8,
                inlen,
                outlen,
            )
        }
    };
    if ret < 0 {
        return Err(Error::eio());
    }
    Ok(ret as usize)
}

fn z_erofs_decompress_zstd(
    _sbi: &SbInfo,
    src: &[u8],
    dst: &mut [u8],
    decodedlength: u64,
    decodedskip: u64,
    partial_decoding: bool,
) -> Result<()> {
    use zstd::zstd_safe::{DCtx, InBuffer, OutBuffer};

    let inputmargin = z_erofs_fixup_insize(src);
    if inputmargin >= src.len() {
        return Err(Error::efscorrupted());
    }

    let mut dctx = DCtx::create();
    dctx.init().map_err(|_| Error::efscorrupted())?;

    fn run_zstd(
        dctx: &mut zstd::zstd_safe::DCtx<'_>,
        input: &[u8],
        output: &mut [u8],
        decodedlength: u64,
        partial_decoding: bool,
    ) -> Result<()> {
        let mut inbuf = InBuffer::around(input);
        let mut outbuf = OutBuffer::around(output);

        let ret = match dctx.decompress_stream(&mut outbuf, &mut inbuf) {
            Ok(r) => r,
            Err(code) => code,
        };
        if unsafe { zstd::zstd_safe::zstd_sys::ZSTD_isError(ret) } != 0 {
            return Err(Error::efscorrupted());
        }

        if partial_decoding {
            if outbuf.pos() < decodedlength as usize {
                return Err(Error::efscorrupted());
            }
        } else {
            if ret != 0 {
                return Err(Error::efscorrupted());
            }
            if outbuf.pos() != decodedlength as usize {
                return Err(Error::efscorrupted());
            }
        }
        Ok(())
    }

    if decodedskip != 0 {
        let mut buff = vec![0u8; decodedlength as usize];
        run_zstd(
            &mut dctx,
            &src[inputmargin..],
            &mut buff,
            decodedlength,
            partial_decoding,
        )?;
        dst.copy_from_slice(&buff[decodedskip as usize..]);
    } else {
        run_zstd(
            &mut dctx,
            &src[inputmargin..],
            dst,
            decodedlength,
            partial_decoding,
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn z_erofs_decompress(
    sbi: &SbInfo,
    src: &[u8],
    dst: &mut [u8],
    decodedskip: u64,
    length: u64,
    interlaced_offset: u64,
    alg: u8,
    partial_decoding: bool,
) -> Result<()> {
    let decodedlength = length;

    if alg == Z_EROFS_COMPRESSION_INTERLACED {
        if decodedlength > src.len() as u64 {
            return Err(Error::eopnotsupp());
        }
        if decodedlength < decodedskip {
            return Err(Error::efscorrupted());
        }
        if src.len() as u64 > sbi.blksiz() as u64 {
            return Err(Error::efscorrupted());
        }
        let count = (decodedlength - decodedskip) as usize;
        let skip = sbi.blkoff(interlaced_offset + decodedskip) as usize;
        let rightpart = std::cmp::min(sbi.blksiz() as usize - skip, count);
        dst[..rightpart].copy_from_slice(&src[skip..skip + rightpart]);
        dst[rightpart..count].copy_from_slice(&src[..count - rightpart]);
        return Ok(());
    } else if alg == Z_EROFS_COMPRESSION_SHIFTED {
        if decodedlength > src.len() as u64 {
            return Err(Error::eopnotsupp());
        }
        if decodedlength < decodedskip {
            return Err(Error::efscorrupted());
        }
        dst.copy_from_slice(&src[decodedskip as usize..decodedlength as usize]);
        return Ok(());
    }

    if alg == Z_EROFS_COMPRESSION_LZ4 {
        return z_erofs_decompress_lz4(src, dst, decodedlength, decodedskip, partial_decoding);
    }
    if alg == Z_EROFS_COMPRESSION_ZSTD {
        return z_erofs_decompress_zstd(
            sbi,
            src,
            dst,
            decodedlength,
            decodedskip,
            partial_decoding,
        );
    }
    Err(Error::eopnotsupp())
}
