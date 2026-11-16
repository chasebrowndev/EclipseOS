// SPDX-License-Identifier: AGPL-3.0-only
//! A CPU solid-colour buffer for a window: the way a window with no GPU surface (tests, the
//! software fallback before wgpu is ready) maps. T-09 §7: never use this on a surface the
//! Vulkan WSI owns.

use std::io::Write;
use std::os::fd::AsFd;

use wayland_client::protocol::wl_buffer::{self, WlBuffer};
use wayland_client::protocol::wl_shm::Format;
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};

use crate::event::WindowId;
use crate::state::{WlError, WlState};

delegate_noop!(WlState: ignore WlShmPool);

impl Dispatch<WlBuffer, ()> for WlState {
    fn event(
        _: &mut Self,
        buffer: &WlBuffer,
        event: wl_buffer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event {
            buffer.destroy();
        }
    }
}

impl WlState {
    /// Attach a `w`x`h` XRGB8888 buffer of one colour to the window and commit.
    pub(crate) fn attach_solid(
        &mut self,
        id: WindowId,
        w: u32,
        h: u32,
        xrgb: u32,
    ) -> Result<(), WlError> {
        let wi = self.win_index(id).ok_or(WlError::UnknownWindow)?;
        let (w, h) = (w.clamp(1, 16_384), h.clamp(1, 16_384));
        let stride = w * 4;
        let len = (stride as usize) * (h as usize);
        let fd = rustix::fs::memfd_create("apoth-wl-solid", rustix::fs::MemfdFlags::CLOEXEC)
            .map_err(std::io::Error::from)?;
        let mut file = std::fs::File::from(fd);
        let row: Vec<u8> = (0..w).flat_map(|_| xrgb.to_le_bytes()).collect();
        for _ in 0..h {
            file.write_all(&row)?;
        }
        let pool = self
            .globals
            .shm
            .create_pool(file.as_fd(), len as i32, &self.qh, ());
        let buffer = pool.create_buffer(
            0,
            w as i32,
            h as i32,
            stride as i32,
            Format::Xrgb8888,
            &self.qh,
            (),
        );
        pool.destroy();
        let surface = &self.windows[wi].surface;
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, w as i32, h as i32);
        surface.commit();
        Ok(())
    }
}
