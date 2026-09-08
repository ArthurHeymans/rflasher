/// Wraps the programmer in a flash device
/// for operations that need chip-level read/write/erase.
///
/// For Dediprog: creates [`HybridFlashDevice`] (fast bulk read/write via
/// `OpaqueMaster`) and calls `set_flash_size()` first.
/// For all others: creates [`SpiFlashDevice`].
///
/// The body receives `$device` as `&mut impl FlashDevice`. The macro handles
/// extracting the master back via `into_parts()` and putting the Programmer
/// wrapper back into shared state. `finish()` runs before teardown so a
/// volatile QE bit set by `prepare()` is restored after every op (the next
/// op re-prepares from scratch).
macro_rules! with_flash_device {
    ($shared:expr, $programmer:expr, $ctx_flash:expr, $device:ident, $failed:ident, $body:expr) => {
        match $programmer {
            Programmer::Serprog(master) => {
                let mut $device = SpiFlashDevice::new(master, $ctx_flash);
                match $device.prepare().await {
                    Ok(()) => $body,
                    Err(error) => {
                        $shared
                            .borrow_mut()
                            .messages
                            .push(AsyncMessage::$failed(format!("{error:?}")));
                    }
                }
                // The device is discarded after this op (master is handed
                // back), so session teardown happens here: restores a
                // volatile QE bit set by prepare(); compatibility EN4B
                // remains active, matching flashprog. Best-effort: the op result stands.
                if let Err(error) = $device.finish().await {
                    log::warn!("flash session teardown (finish) failed: {error}");
                }
                let (master, _) = $device.into_parts();
                $shared.borrow_mut().programmer = Some(Programmer::Serprog(master));
            }
            Programmer::Ch341a(master) => {
                let mut $device = SpiFlashDevice::new(master, $ctx_flash);
                match $device.prepare().await {
                    Ok(()) => $body,
                    Err(error) => {
                        $shared
                            .borrow_mut()
                            .messages
                            .push(AsyncMessage::$failed(format!("{error:?}")));
                    }
                }
                // The device is discarded after this op (master is handed
                // back), so session teardown happens here: restores a
                // volatile QE bit set by prepare(); compatibility EN4B
                // remains active, matching flashprog. Best-effort: the op result stands.
                if let Err(error) = $device.finish().await {
                    log::warn!("flash session teardown (finish) failed: {error}");
                }
                let (master, _) = $device.into_parts();
                $shared.borrow_mut().programmer = Some(Programmer::Ch341a(master));
            }
            Programmer::Ch347(master) => {
                let mut $device = SpiFlashDevice::new(master, $ctx_flash);
                match $device.prepare().await {
                    Ok(()) => $body,
                    Err(error) => {
                        $shared
                            .borrow_mut()
                            .messages
                            .push(AsyncMessage::$failed(format!("{error:?}")));
                    }
                }
                // The device is discarded after this op (master is handed
                // back), so session teardown happens here: restores a
                // volatile QE bit set by prepare(); compatibility EN4B
                // remains active, matching flashprog. Best-effort: the op result stands.
                if let Err(error) = $device.finish().await {
                    log::warn!("flash session teardown (finish) failed: {error}");
                }
                let (master, _) = $device.into_parts();
                $shared.borrow_mut().programmer = Some(Programmer::Ch347(master));
            }
            Programmer::Ftdi(master) => {
                let mut $device = SpiFlashDevice::new(master, $ctx_flash);
                match $device.prepare().await {
                    Ok(()) => $body,
                    Err(error) => {
                        $shared
                            .borrow_mut()
                            .messages
                            .push(AsyncMessage::$failed(format!("{error:?}")));
                    }
                }
                // The device is discarded after this op (master is handed
                // back), so session teardown happens here: restores a
                // volatile QE bit set by prepare(); compatibility EN4B
                // remains active, matching flashprog. Best-effort: the op result stands.
                if let Err(error) = $device.finish().await {
                    log::warn!("flash session teardown (finish) failed: {error}");
                }
                let (master, _) = $device.into_parts();
                $shared.borrow_mut().programmer = Some(Programmer::Ftdi(master));
            }
            Programmer::Ft4222(master) => {
                let mut $device = SpiFlashDevice::new(master, $ctx_flash);
                match $device.prepare().await {
                    Ok(()) => $body,
                    Err(error) => {
                        $shared
                            .borrow_mut()
                            .messages
                            .push(AsyncMessage::$failed(format!("{error:?}")));
                    }
                }
                // The device is discarded after this op (master is handed
                // back), so session teardown happens here: restores a
                // volatile QE bit set by prepare(); compatibility EN4B
                // remains active, matching flashprog. Best-effort: the op result stands.
                if let Err(error) = $device.finish().await {
                    log::warn!("flash session teardown (finish) failed: {error}");
                }
                let (master, _) = $device.into_parts();
                $shared.borrow_mut().programmer = Some(Programmer::Ft4222(master));
            }
            Programmer::Dediprog(mut master) => {
                master.set_flash_size($ctx_flash.total_size() as u32);
                let mut $device = HybridFlashDevice::new(master, $ctx_flash);
                match $device.prepare().await {
                    Ok(()) => $body,
                    Err(error) => {
                        $shared
                            .borrow_mut()
                            .messages
                            .push(AsyncMessage::$failed(format!("{error:?}")));
                    }
                }
                // The device is discarded after this op (master is handed
                // back), so session teardown happens here: restores a
                // volatile QE bit set by prepare(); compatibility EN4B
                // remains active, matching flashprog. Best-effort: the op result stands.
                if let Err(error) = $device.finish().await {
                    log::warn!("flash session teardown (finish) failed: {error}");
                }
                let (master, _) = $device.into_parts();
                $shared.borrow_mut().programmer = Some(Programmer::Dediprog(master));
            }
            Programmer::Raiden(master) => {
                let mut $device = SpiFlashDevice::new(master, $ctx_flash);
                match $device.prepare().await {
                    Ok(()) => $body,
                    Err(error) => {
                        $shared
                            .borrow_mut()
                            .messages
                            .push(AsyncMessage::$failed(format!("{error:?}")));
                    }
                }
                // The device is discarded after this op (master is handed
                // back), so session teardown happens here: restores a
                // volatile QE bit set by prepare(); compatibility EN4B
                // remains active, matching flashprog. Best-effort: the op result stands.
                if let Err(error) = $device.finish().await {
                    log::warn!("flash session teardown (finish) failed: {error}");
                }
                let (master, _) = $device.into_parts();
                $shared.borrow_mut().programmer = Some(Programmer::Raiden(master));
            }
        }
    };
}

pub(crate) use with_flash_device;
