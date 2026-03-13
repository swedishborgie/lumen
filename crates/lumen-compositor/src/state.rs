use std::fs::File;
use std::time::Instant;

use gbm::{BufferObject, Device as RawGbmDevice};
use smithay::{
    backend::{
        allocator::dmabuf::Dmabuf,
        renderer::{
            gles::GlesRenderer,
            pixman::PixmanRenderer,
        },
    },
    desktop::{PopupManager, Space, Window},
    input::{
        pointer::CursorImageStatus,
        Seat, SeatState,
    },
    output::Output,
    reexports::wayland_server::DisplayHandle,
    utils::{Clock, Monotonic},
    wayland::{
        compositor::CompositorState,
        dmabuf::{DmabufGlobal, DmabufState},
        fractional_scale::FractionalScaleManagerState,
        foreign_toplevel_list::ForeignToplevelListState,
        output::OutputManagerState,
        pointer_constraints::PointerConstraintsState,
        pointer_warp::PointerWarpManager,
        presentation::PresentationState,
        relative_pointer::RelativePointerManagerState,
        selection::{
            data_device::DataDeviceState,
            primary_selection::PrimarySelectionState,
            wlr_data_control::DataControlState,
        },
        shell::{wlr_layer::WlrLayerShellState, xdg::XdgShellState, xdg::decoration::XdgDecorationState},
        shm::ShmState,
        single_pixel_buffer::SinglePixelBufferState,
        virtual_keyboard::VirtualKeyboardManagerState,
        viewporter::ViewporterState,
        xdg_activation::XdgActivationState,
    },
};
use tokio::sync::broadcast;

use crate::input::InputEvent;
use crate::types::CapturedFrame;

/// Internal commands sent into the calloop event loop.
pub enum CompositorCommand {
    Input(InputEvent),
    Stop,
}

/// All Smithay state in one struct, owned by the calloop event loop.
pub struct AppState {
    pub compositor_state: CompositorState,
    pub fractional_scale_state: FractionalScaleManagerState,
    pub viewporter_state: ViewporterState,
    pub presentation_state: PresentationState,
    pub shm_state: ShmState,
    pub single_pixel_buffer: SinglePixelBufferState,
    pub dmabuf_state: DmabufState,
    pub dmabuf_global: Option<DmabufGlobal>,
    pub output_state: OutputManagerState,
    pub seat_state: SeatState<AppState>,
    pub shell_state: XdgShellState,
    pub layer_shell_state: WlrLayerShellState,
    pub data_device_state: DataDeviceState,
    pub data_control_state: DataControlState,
    pub virtual_keyboard_state: VirtualKeyboardManagerState,
    pub pointer_warp_state: PointerWarpManager,
    pub relative_pointer_state: RelativePointerManagerState,
    pub pointer_constraints_state: PointerConstraintsState,
    pub foreign_toplevel_list: ForeignToplevelListState,
    pub xdg_decoration_state: XdgDecorationState,
    pub xdg_activation_state: XdgActivationState,
    pub primary_selection_state: PrimarySelectionState,
    pub popups: PopupManager,

    pub dh: DisplayHandle,
    pub seat: Seat<AppState>,
    pub outputs: Vec<Output>,
    pub space: Space<Window>,
    pub pending_windows: Vec<Window>,

    pub gles_renderer: Option<GlesRenderer>,
    pub pixman_renderer: Option<PixmanRenderer>,
    pub gbm_device: Option<RawGbmDevice<File>>,
    pub offscreen_buffer: Option<(BufferObject<()>, Dmabuf)>,
    pub use_gpu: bool,
    pub frame_buffer: Vec<u8>,

    pub is_capturing: bool,
    pub width: i32,
    pub height: i32,
    pub target_fps: f64,
    pub frame_tx: broadcast::Sender<CapturedFrame>,
    pub frame_counter: u64,
    pub clock: Clock<Monotonic>,

    pub current_cursor_icon: Option<CursorImageStatus>,

    pub last_log_time: Instant,
    pub encoded_frame_count: u32,
    pub start_time: Instant,
}

/// Per-client data stored by the Wayland server.
#[derive(Default)]
pub struct ClientState {
    pub compositor_client_state: smithay::wayland::compositor::CompositorClientState,
}

impl smithay::reexports::wayland_server::backend::ClientData for ClientState {
    fn initialized(&self, _client_id: smithay::reexports::wayland_server::backend::ClientId) {}
    fn disconnected(
        &self,
        _client_id: smithay::reexports::wayland_server::backend::ClientId,
        reason: smithay::reexports::wayland_server::backend::DisconnectReason,
    ) {
        tracing::info!("Wayland client disconnected: {:?}", reason);
    }
}

/// Build a `Dmabuf` from a GBM buffer object.
pub(crate) fn create_dmabuf_from_bo(bo: &BufferObject<()>) -> Dmabuf {
    use smithay::backend::allocator::{dmabuf::DmabufFlags, Fourcc, Modifier};

    let fd = bo.fd().expect("Failed to get GBM BO fd");
    let width = bo.width() as i32;
    let height = bo.height() as i32;
    let stride = bo.stride();
    let modifier = Modifier::from(u64::from(bo.modifier()));

    let mut builder = Dmabuf::builder(
        (width, height),
        Fourcc::Argb8888,
        modifier,
        DmabufFlags::empty(),
    );
    builder.add_plane(fd, 0, 0, stride);
    builder.build().expect("Failed to build dmabuf")
}
