use wrist_bluenrg2::Scheduler;
use wrist_ble::{Activities, Change, ConnParams, Schedule};
use wrist_ble_shadow::menu::MENU;

fn central(a: &Activities, p: ConnParams) -> Activities {
    a.walk(&Scheduler, &[Change::Initiate(Some(p)), Change::Connected]).unwrap()
}

/// Every advertising option on the menu fits beside the menu's own central links, both live
/// intervals and the connecting one, in either order.
#[test]
fn the_menu_fits_the_chips_scheduler() {
    for choice in MENU.advertising.iter().filter(|c| c.when.beside_central != Some(false)) {
        let with_adv = Activities { advertising: Some(choice.params), ..Activities::IDLE };
        for link in MENU.connecting.iter().chain(MENU.live) {
            assert!(central(&with_adv, link.params).central[0].is_some());
            let b = central(&Activities::IDLE, link.params);
            assert!(Scheduler.admit(&b, &Change::Advertise(Some(choice.params))).is_ok(), "{choice:?} beside {link:?}");
        }
        let connecting = central(&with_adv, MENU.connecting[0].params);
        for live in MENU.live {
            assert!(connecting.walk(&Scheduler, &[Change::Retime { slot: 0, params: live.params }]).is_ok(), "{live:?} beside {choice:?}");
        }
    }
}
