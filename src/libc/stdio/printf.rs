/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `printf` function family. The implementation is also used by `NSLog` etc.

use crate::abi::{DotDotDot, VaList};
use crate::dyld::{export_c_func, FunctionExports};
use crate::frameworks::foundation::{ns_string, unichar};
use crate::libc::posix_io::{STDERR_FILENO, STDOUT_FILENO};
use crate::libc::stdio::FILE;
use crate::mem::{ConstPtr, GuestUSize, Mem, MutPtr, MutVoidPtr};
use crate::objc::{id, msg};
use crate::Environment;
use std::collections::HashSet;
use std::io::Write;

const INTEGER_SPECIFIERS: [u8; 6] = [b'd', b'i', b'o', b'u', b'x', b'X'];
const FLOAT_SPECIFIERS: [u8; 1] = [b'f'];

/// String formatting implementation for `printf` and `NSLog` function families.
///
/// `NS_LOG` is [true] for the `NSLog` format string type, or [false] for the
/// `printf` format string type.
///
/// `get_format_char` is a callback that returns the byte at a given index in
/// the format string, or `'\0'` if the index is one past the last byte.
pub fn printf_inner<const NS_LOG: bool, F: Fn(&Mem, GuestUSize) -> u8>(
    env: &mut Environment,
    get_format_char: F,
    mut args: VaList,
) -> Vec<u8> {
    let mut res = Vec::<u8>::new();

    let mut format_char_idx = 0;

    loop {
        let c = get_format_char(&env.mem, format_char_idx);
        format_char_idx += 1;

        if c == b'\0' {
            break;
        }
        if c != b'%' {
            res.push(c);
            continue;
        }

        let pad_char = if get_format_char(&env.mem, format_char_idx) == b'0' {
            format_char_idx += 1;
            '0'
        } else {
            ' '
        };

        let pad_width = if get_format_char(&env.mem, format_char_idx) == b'*' {
            let pad_width = args.next::<i32>(env);
            assert!(pad_width >= 0); // TODO: Implement right-padding
            format_char_idx += 1;
            pad_width
        } else {
            let mut pad_width: i32 = 0;
            while let c @ b'0'..=b'9' = get_format_char(&env.mem, format_char_idx) {
                pad_width = pad_width * 10 + (c - b'0') as i32;
                format_char_idx += 1;
            }
            pad_width
        };

        let precision = if get_format_char(&env.mem, format_char_idx) == b'.' {
            format_char_idx += 1;
            let mut precision = 0;
            while let c @ b'0'..=b'9' = get_format_char(&env.mem, format_char_idx) {
                precision = precision * 10 + (c - b'0') as usize;
                format_char_idx += 1;
            }
            Some(precision)
        } else {
            None
        };

        let length_modifier = if get_format_char(&env.mem, format_char_idx) == b'l' {
            format_char_idx += 1;
            Some(b'l')
        } else {
            None
        };

        let specifier = get_format_char(&env.mem, format_char_idx);
        format_char_idx += 1;

        assert!(specifier != b'\0');
        if specifier == b'%' {
            res.push(b'%');
            continue;
        }

        if precision.is_some() {
            assert!(
                INTEGER_SPECIFIERS.contains(&specifier) || FLOAT_SPECIFIERS.contains(&specifier)
            )
        }

        match specifier {
            b'c' => {
                // TODO: support length modifier
                assert!(length_modifier.is_none());
                let c: u8 = args.next(env);
                assert!(pad_char == ' ' && pad_width == 0); // TODO
                res.push(c);
            }
            // Apple extension? Seemingly works in both NSLog and printf.
            b'C' => {
                assert!(length_modifier.is_none());
                let c: unichar = args.next(env);
                // TODO
                assert!(pad_char == ' ' && pad_width == 0);
                // This will panic if it's a surrogate! This isn't good if
                // targeting UTF-16 ([NSString stringWithFormat:] etc).
                let c = char::from_u32(c.into()).unwrap();
                write!(&mut res, "{}", c).unwrap();
            }
            b's' => {
                // TODO: support length modifier
                assert!(length_modifier.is_none());
                let c_string: ConstPtr<u8> = args.next(env);
                assert!(pad_char == ' ' && pad_width == 0); // TODO
                if !c_string.is_null() {
                    res.extend_from_slice(env.mem.cstr_at(c_string));
                } else {
                    res.extend_from_slice("(null)".as_bytes());
                }
            }
            b'd' | b'i' | b'u' => {
                // Note: on 32-bit system int and long are i32,
                // so length_modifier is ignored
                let int: i64 = if specifier == b'u' {
                    let uint: u32 = args.next(env);
                    uint.into()
                } else {
                    let int: i32 = args.next(env);
                    int.into()
                };

                let int_with_precision = if precision.is_some_and(|value| value > 0) {
                    format!("{:01$}", int, precision.unwrap())
                } else {
                    format!("{}", int)
                };

                if pad_width > 0 {
                    let pad_width = pad_width as usize;
                    if pad_char == '0' && precision.is_none() {
                        write!(&mut res, "{:0>1$}", int_with_precision, pad_width).unwrap();
                    } else {
                        write!(&mut res, "{:>1$}", int_with_precision, pad_width).unwrap();
                    }
                } else {
                    res.extend_from_slice(int_with_precision.as_bytes());
                }
            }
            b'f' => {
                // TODO: support length modifier
                assert!(length_modifier.is_none());
                let float: f64 = args.next(env);
                let precision_value = precision.unwrap_or(6);
                if pad_width > 0 {
                    let pad_width = pad_width as usize;
                    if pad_char == '0' {
                        write!(&mut res, "{:01$.2$}", float, pad_width, precision_value).unwrap();
                    } else {
                        write!(&mut res, "{:1$.2$}", float, pad_width, precision_value).unwrap();
                    }
                } else {
                    write!(&mut res, "{:.1$}", float, precision_value).unwrap();
                }
            }
            b'@' if NS_LOG => {
                assert!(length_modifier.is_none());
                let object: id = args.next(env);
                // TODO: use localized description if available?
                let description: id = msg![env; object description];
                // TODO: avoid copy
                // TODO: what if the description isn't valid UTF-16?
                let description = ns_string::to_rust_string(env, description);
                write!(&mut res, "{}", description).unwrap();
            }
            b'x' => {
                // Note: on 32-bit system unsigned int and unsigned long
                // are u32, so length_modifier is ignored
                let uint: u32 = args.next(env);
                res.extend_from_slice(format!("{:x}", uint).as_bytes());
            }
            b'X' => {
                // Note: on 32-bit system unsigned int and unsigned long
                // are u32, so length_modifier is ignored
                let uint: u32 = args.next(env);
                res.extend_from_slice(format!("{:X}", uint).as_bytes());
            }
            b'p' => {
                assert!(length_modifier.is_none());
                let ptr: MutVoidPtr = args.next(env);
                res.extend_from_slice(format!("{:?}", ptr).as_bytes());
            }
            // TODO: more specifiers
            _ => unimplemented!(
                "Format character '{}'. Formatted up to index {}",
                specifier as char,
                format_char_idx
            ),
        }
    }

    log_dbg!("=> {:?}", std::str::from_utf8(&res));

    res
}

fn snprintf(
    env: &mut Environment,
    dest: MutPtr<u8>,
    n: GuestUSize,
    format: ConstPtr<u8>,
    args: DotDotDot,
) -> i32 {
    vsnprintf(env, dest, n, format, args.start())
}

fn vprintf(env: &mut Environment, format: ConstPtr<u8>, arg: VaList) -> i32 {
    log_dbg!(
        "vprintf({:?} ({:?}), ...)",
        format,
        env.mem.cstr_at_utf8(format)
    );

    let res = printf_inner::<false, _>(env, |mem, idx| mem.read(format + idx), arg);
    // TODO: I/O error handling
    let _ = std::io::stdout().write_all(&res);
    res.len().try_into().unwrap()
}

fn vsnprintf(
    env: &mut Environment,
    dest: MutPtr<u8>,
    n: GuestUSize,
    format: ConstPtr<u8>,
    arg: VaList,
) -> i32 {
    log_dbg!(
        "vsnprintf({:?} {:?} {:?})",
        dest,
        format,
        env.mem.cstr_at_utf8(format)
    );

    let res = printf_inner::<false, _>(env, |mem, idx| mem.read(format + idx), arg);
    let middle = if ((n - 1) as usize) < res.len() {
        &res[..(n - 1) as usize]
    } else {
        &res[..]
    };

    let dest_slice = env.mem.bytes_at_mut(dest, n);
    for (i, &byte) in middle.iter().chain(b"\0".iter()).enumerate() {
        dest_slice[i] = byte;
    }

    res.len().try_into().unwrap()
}

fn vsprintf(env: &mut Environment, dest: MutPtr<u8>, format: ConstPtr<u8>, arg: VaList) -> i32 {
    log_dbg!(
        "vsprintf({:?}, {:?} ({:?}), ...)",
        dest,
        format,
        env.mem.cstr_at_utf8(format)
    );

    let res = printf_inner::<false, _>(env, |mem, idx| mem.read(format + idx), arg);

    let dest_slice = env
        .mem
        .bytes_at_mut(dest, (res.len() + 1).try_into().unwrap());
    for (i, &byte) in res.iter().chain(b"\0".iter()).enumerate() {
        dest_slice[i] = byte;
    }

    res.len().try_into().unwrap()
}

fn sprintf(env: &mut Environment, dest: MutPtr<u8>, format: ConstPtr<u8>, args: DotDotDot) -> i32 {
    log_dbg!(
        "sprintf({:?}, {:?} ({:?}), ...)",
        dest,
        format,
        env.mem.cstr_at_utf8(format)
    );

    let res = printf_inner::<false, _>(env, |mem, idx| mem.read(format + idx), args.start());

    let dest_slice = env
        .mem
        .bytes_at_mut(dest, (res.len() + 1).try_into().unwrap());
    for (i, &byte) in res.iter().chain(b"\0".iter()).enumerate() {
        dest_slice[i] = byte;
    }

    res.len().try_into().unwrap()
}

fn printf(env: &mut Environment, format: ConstPtr<u8>, args: DotDotDot) -> i32 {
    log_dbg!(
        "printf({:?} ({:?}), ...)",
        format,
        env.mem.cstr_at_utf8(format)
    );

    let res = printf_inner::<false, _>(env, |mem, idx| mem.read(format + idx), args.start());
    // TODO: I/O error handling
    let _ = std::io::stdout().write_all(&res);
    res.len().try_into().unwrap()
}

// TODO: more printf variants

fn sscanf(env: &mut Environment, src: ConstPtr<u8>, format: ConstPtr<u8>, args: DotDotDot) -> i32 {
    log_dbg!(
        "sscanf({:?} ({:?}), {:?} ({:?}), ...)",
        src,
        env.mem.cstr_at_utf8(src),
        format,
        env.mem.cstr_at_utf8(format)
    );

    let mut args = args.start();

    let mut src_ptr = src.cast_mut();
    let mut format_char_idx = 0;

    let mut matched_args = 0;

    loop {
        let c = env.mem.read(format + format_char_idx);
        format_char_idx += 1;

        if c == b'\0' {
            break;
        }
        if c != b'%' {
            let cc = env.mem.read(src_ptr);
            if c != cc {
                return matched_args - 1;
            }
            src_ptr += 1;
            continue;
        }

        let length_modifier = if env.mem.read(format + format_char_idx) == b'h' {
            format_char_idx += 1;
            Some(b'h')
        } else {
            None
        };

        let specifier = env.mem.read(format + format_char_idx);
        format_char_idx += 1;

        match specifier {
            b'd' | b'i' => {
                if specifier == b'i' {
                    // TODO: hexs and octals
                    assert_ne!(env.mem.read(src_ptr), b'0');
                }

                match length_modifier {
                    Some(lm) => {
                        match lm {
                            b'h' => {
                                // signed short* or unsigned short*
                                let mut val: i16 = 0;
                                while let c @ b'0'..=b'9' = env.mem.read(src_ptr) {
                                    val = val * 10 + (c - b'0') as i16;
                                    src_ptr += 1;
                                }
                                let c_short_ptr: ConstPtr<i16> = args.next(env);
                                env.mem.write(c_short_ptr.cast_mut(), val);
                            }
                            _ => unimplemented!(),
                        }
                    }
                    _ => {
                        let mut val: i32 = 0;
                        while let c @ b'0'..=b'9' = env.mem.read(src_ptr) {
                            val = val * 10 + (c - b'0') as i32;
                            src_ptr += 1;
                        }
                        let c_int_ptr: ConstPtr<i32> = args.next(env);
                        env.mem.write(c_int_ptr.cast_mut(), val);
                    }
                }
            }
            b'[' => {
                assert!(length_modifier.is_none());
                // TODO: support ranges like [0-9]
                // [set] case
                let mut c = env.mem.read(format + format_char_idx);
                format_char_idx += 1;
                // TODO: only `not in the set` for a moment
                assert_eq!(c, b'^');
                // Build set
                let mut set: HashSet<u8> = HashSet::new();
                // TODO: set can contain ']' as well
                c = env.mem.read(format + format_char_idx);
                format_char_idx += 1;
                while c != b']' {
                    set.insert(c);
                    c = env.mem.read(format + format_char_idx);
                    format_char_idx += 1;
                }
                let mut dst_ptr: MutPtr<u8> = args.next(env);
                // Consume `src` while chars are not in the set
                let mut cc = env.mem.read(src_ptr);
                src_ptr += 1;
                // TODO: handle end of src string
                while !set.contains(&cc) {
                    env.mem.write(dst_ptr, cc);
                    dst_ptr += 1;
                    cc = env.mem.read(src_ptr);
                    src_ptr += 1;
                }
                // we need to backtrack one position
                src_ptr -= 1;
                env.mem.write(dst_ptr, b'\0');
            }
            // TODO: more specifiers
            _ => unimplemented!("Format character '{}'", specifier as char),
        }

        matched_args += 1;
    }

    matched_args
}

fn fprintf(
    env: &mut Environment,
    stream: MutPtr<FILE>,
    format: ConstPtr<u8>,
    args: DotDotDot,
) -> i32 {
    log_dbg!(
        "fprintf({:?}, {:?} ({:?}), ...)",
        stream,
        format,
        env.mem.cstr_at_utf8(format)
    );

    let res = printf_inner::<false, _>(env, |mem, idx| mem.read(format + idx), args.start());
    // TODO: I/O error handling
    match env.mem.read(stream).fd {
        STDOUT_FILENO => _ = std::io::stdout().write_all(&res),
        STDERR_FILENO => _ = std::io::stderr().write_all(&res),
        _ => unimplemented!(),
    }
    res.len().try_into().unwrap()
}

pub const FUNCTIONS: FunctionExports = &[
    export_c_func!(sscanf(_, _, _)),
    export_c_func!(snprintf(_, _, _, _)),
    export_c_func!(vprintf(_, _)),
    export_c_func!(vsnprintf(_, _, _, _)),
    export_c_func!(vsprintf(_, _, _)),
    export_c_func!(sprintf(_, _, _)),
    export_c_func!(printf(_, _)),
    export_c_func!(fprintf(_, _, _)),
];
