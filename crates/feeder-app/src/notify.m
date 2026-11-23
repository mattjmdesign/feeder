#import <AppKit/AppKit.h>
#import <UserNotifications/UserNotifications.h>

@interface FeederNotifier : NSObject <UNUserNotificationCenterDelegate>
@end

@implementation FeederNotifier

- (void)userNotificationCenter:(UNUserNotificationCenter *)center
       willPresentNotification:(UNNotification *)notification
         withCompletionHandler:(void (^)(UNNotificationPresentationOptions))completionHandler {
    completionHandler(UNNotificationPresentationOptionBanner | UNNotificationPresentationOptionList);
}

- (void)userNotificationCenter:(UNUserNotificationCenter *)center
    didReceiveNotificationResponse:(UNNotificationResponse *)response
             withCompletionHandler:(void (^)(void))completionHandler {
    NSString *link = response.notification.request.content.userInfo[@"url"];
    NSURL *url = link.length > 0 ? [NSURL URLWithString:link] : nil;
    if (url != nil && [[url.scheme lowercaseString] isEqualToString:@"feeder"]) {
        [NSWorkspace.sharedWorkspace openURL:url];
    }
    completionHandler();
}

@end

static FeederNotifier *notifier;

static bool bundled(void) {
    return NSBundle.mainBundle.bundleIdentifier != nil;
}

void feeder_notify_start(void) {
    if (!bundled() || notifier != nil) {
        return;
    }
    notifier = [FeederNotifier new];
    UNUserNotificationCenter.currentNotificationCenter.delegate = notifier;
}

bool feeder_notify(const char *title, const char *body, const char *url) {
    if (!bundled()) {
        return false;
    }
    feeder_notify_start();
    NSString *titleText = (title != NULL ? [NSString stringWithUTF8String:title] : nil) ?: @"Feeder";
    NSString *bodyText = (body != NULL ? [NSString stringWithUTF8String:body] : nil) ?: @"";
    NSString *link = url != NULL ? [NSString stringWithUTF8String:url] : nil;
    UNUserNotificationCenter *center = UNUserNotificationCenter.currentNotificationCenter;
    [center requestAuthorizationWithOptions:UNAuthorizationOptionAlert
                          completionHandler:^(BOOL granted, __unused NSError *error) {
        if (!granted) {
            return;
        }
        UNMutableNotificationContent *content = [UNMutableNotificationContent new];
        content.title = titleText;
        content.body = bodyText;
        if (link != nil) {
            content.userInfo = @{@"url": link};
        }
        UNNotificationRequest *request = [UNNotificationRequest requestWithIdentifier:NSUUID.UUID.UUIDString
                                                                              content:content
                                                                              trigger:nil];
        [center addNotificationRequest:request withCompletionHandler:nil];
    }];
    return true;
}
