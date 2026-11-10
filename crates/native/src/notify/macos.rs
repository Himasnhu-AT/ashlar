//! macOS: UNUserNotificationCenter and its delegate.
#![allow(unsafe_code)]

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Once;

use block2::{DynBlock, RcBlock};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AnyThread, define_class, msg_send};
use objc2_foundation::{NSArray, NSBundle, NSDictionary, NSError, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotification, UNNotificationPresentationOptions, UNNotificationRequest,
    UNNotificationResponse, UNNotificationSound, UNUserNotificationCenter, UNUserNotificationCenterDelegate,
};

use super::{Click, Prepared, decode_launch, deliver, encode_launch};
use crate::{NativeError, Result};

/// The user-info key that carries [`encode_launch`]'s string.
const INFO_KEY: &str = "ashlar";

/// Whether this process is a bundled app the notification center accepts.
fn bundled() -> bool {
    let b = NSBundle::mainBundle();
    b.bundleIdentifier().is_some() && b.bundlePath().to_string().ends_with(".app")
}

fn click_of(response: &UNNotificationResponse) -> Click {
    let request = response.notification().request();
    let id = request.identifier().to_string();
    let key = NSString::from_str(INFO_KEY);
    let key: &AnyObject = key.as_ref();
    let text = request.content().userInfo().objectForKey(key).and_then(|o| o.downcast::<NSString>().ok()).map(|s| s.to_string());
    let info = text.and_then(|t| decode_launch(&t)).map(|c| c.info).unwrap_or_default();
    Click { id, info }
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and this class adds no ivars and
    // implements no `Drop`.
    #[unsafe(super(NSObject))]
    #[name = "AshlarNativeNotificationDelegate"]
    struct Delegate;

    // SAFETY: NSObject implements NSObjectProtocol.
    unsafe impl NSObjectProtocol for Delegate {}

    // SAFETY: both methods are optional protocol methods, declared with the protocol's signatures.
    unsafe impl UNUserNotificationCenterDelegate for Delegate {
        // SAFETY: matches `userNotificationCenter:willPresentNotification:withCompletionHandler:`.
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present(
            &self,
            _center: &UNUserNotificationCenter,
            notification: &UNNotification,
            handler: &DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            let shown = UNNotificationPresentationOptions::Banner | UNNotificationPresentationOptions::List;
            // No panic may unwind into the notification center.
            let options = catch_unwind(AssertUnwindSafe(|| {
                if notification.request().content().sound().is_some() { shown | UNNotificationPresentationOptions::Sound } else { shown }
            }))
            .unwrap_or(shown);
            handler.call((options,));
        }

        // SAFETY: matches `userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:`.
        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn did_receive(&self, _center: &UNUserNotificationCenter, response: &UNNotificationResponse, handler: &DynBlock<dyn Fn()>) {
            let _ = catch_unwind(AssertUnwindSafe(|| deliver(click_of(response))));
            handler.call(());
        }
    }
);

impl Delegate {
    fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(());
        // SAFETY: `init` is NSObject's designated initializer, called once on a fresh allocation.
        unsafe { msg_send![super(this), init] }
    }
}

/// Install the delegate once. Only call it in a bundled app.
fn install() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let delegate = Delegate::new();
        UNUserNotificationCenter::currentNotificationCenter().setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        // The center holds its delegate weakly; this one lives for the rest of the process.
        std::mem::forget(delegate);
    });
}

pub fn show(p: &Prepared) -> Result<()> {
    if !bundled() {
        return Err(NativeError::Unsupported);
    }
    install();
    let p = p.clone();
    let mut options = UNAuthorizationOptions::Alert;
    if !p.silent {
        options |= UNAuthorizationOptions::Sound;
    }
    let done = RcBlock::new(move |granted: Bool, _err: *mut NSError| {
        let _ = catch_unwind(|| {
            if !granted.as_bool() {
                log::info!("notifications: not allowed by the user");
                return;
            }
            let content = UNMutableNotificationContent::new();
            content.setTitle(&NSString::from_str(&p.title));
            content.setBody(&NSString::from_str(&p.body));
            if !p.silent {
                content.setSound(Some(&UNNotificationSound::defaultSound()));
            }
            let key = NSString::from_str(INFO_KEY);
            let value = NSString::from_str(&encode_launch(&p.id, &p.info));
            let info = NSDictionary::<NSString, NSString>::from_slices(&[&*key], &[&*value]);
            // SAFETY: user info must be a property-list dictionary; this one maps a string to a
            // string, and the cast only erases those types.
            unsafe { content.setUserInfo(info.cast_unchecked()) };
            // The same identifier replaces a notification already shown.
            let id = NSString::from_str(&p.id);
            let request = UNNotificationRequest::requestWithIdentifier_content_trigger(&id, &content, None);
            UNUserNotificationCenter::currentNotificationCenter().addNotificationRequest_withCompletionHandler(&request, None);
        });
    });
    UNUserNotificationCenter::currentNotificationCenter().requestAuthorizationWithOptions_completionHandler(options, &done);
    Ok(())
}

pub fn remove(ids: &[String]) -> Result<()> {
    if !bundled() {
        return Err(NativeError::Unsupported);
    }
    let ids: Vec<Retained<NSString>> = ids.iter().map(|s| NSString::from_str(s)).collect();
    let ids = NSArray::from_retained_slice(&ids);
    let center = UNUserNotificationCenter::currentNotificationCenter();
    center.removePendingNotificationRequestsWithIdentifiers(&ids);
    center.removeDeliveredNotificationsWithIdentifiers(&ids);
    Ok(())
}

pub fn listen() -> Result<()> {
    if !bundled() {
        return Err(NativeError::Unsupported);
    }
    install();
    Ok(())
}
