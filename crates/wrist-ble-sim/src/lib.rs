//! A fake BlueNRG-2 and the devices around it on virtual time, to run the Bluetooth core on the
//! host against the chip's documented behaviour.

pub mod fake;
pub mod peers;
pub mod rng;

/// Runs a future that never waits, as every one the fake returns.
///
/// # Panics
///
/// The future waited.
pub fn now<F: core::future::Future>(f: F) -> F::Output {
    let mut f = core::pin::pin!(f);
    let mut cx = core::task::Context::from_waker(core::task::Waker::noop());
    match f.as_mut().poll(&mut cx) {
        core::task::Poll::Ready(v) => v,
        core::task::Poll::Pending => panic!("a fake's future waited"),
    }
}
