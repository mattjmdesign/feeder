#import <AppKit/AppKit.h>
#import <Sparkle/Sparkle.h>
#import <libproc.h>
#import <unistd.h>

typedef enum : uint32_t {
    Idle = 0,
    Checking = 1,
    Available = 2,
    Downloading = 3,
    Preparing = 4,
    Ready = 5,
    Installing = 6,
    Current = 7,
    Problem = 8,
    Informational = 9,
} Phase;

typedef struct {
    Phase phase;
    double progress;
    const char *version;
    const char *notes;
    const char *message;
    bool reveal;
    bool can_advance;
} UpdateSnapshot;

@interface FeederUpdateDriver : NSObject <SPUUserDriver, NSMenuItemValidation>
@property (nonatomic) Phase phase;
@property (nonatomic) uint64_t received;
@property (nonatomic) uint64_t expected;
@property (nonatomic) double progress;
@property (nonatomic) BOOL reveal;
@property (nonatomic, copy) NSString *version;
@property (nonatomic, copy) NSString *notes;
@property (nonatomic, copy) NSString *message;
@property (nonatomic, copy) void (^action)(void);
@end

static FeederUpdateDriver *driver;
static SPUUpdater *updater;
static void (*wakeFeeder)(void);

@implementation FeederUpdateDriver

- (void)show:(Phase)phase {
    self.phase = phase;
    wakeFeeder();
}

- (BOOL)hasUpdateDetails {
    switch (self.phase) {
        case Available:
        case Downloading:
        case Preparing:
        case Ready:
        case Installing:
            return YES;
        default:
            return NO;
    }
}

- (BOOL)validateMenuItem:(NSMenuItem *)item {
    return [self hasUpdateDetails] || updater.canCheckForUpdates;
}

- (void)checkForUpdates:(id)sender {
    if ([self hasUpdateDetails]) {
        self.reveal = YES;
        wakeFeeder();
        return;
    }
    if (!updater.canCheckForUpdates) {
        return;
    }
    self.reveal = YES;
    self.message = nil;
    [self show:Checking];
    [updater checkForUpdates];
}

- (void)showUpdatePermissionRequest:(SPUUpdatePermissionRequest *)request reply:(void (^)(SUUpdatePermissionResponse *))reply {
    reply([[SUUpdatePermissionResponse alloc] initWithAutomaticUpdateChecks:YES automaticUpdateDownloading:@NO sendSystemProfile:NO]);
}

- (void)showUserInitiatedUpdateCheckWithCancellation:(void (^)(void))cancellation {
    self.action = nil;
    self.message = nil;
    [self show:Checking];
}

- (void)showUpdateFoundWithAppcastItem:(SUAppcastItem *)item state:(SPUUserUpdateState *)state reply:(void (^)(SPUUserUpdateChoice))reply {
    self.version = item.displayVersionString;
    self.notes = item.itemDescription;
    self.message = nil;
    if (item.informationOnlyUpdate) {
        self.message = @"This release doesn't offer an in-app download.";
        [self show:Informational];
        reply(SPUUserUpdateChoiceDismiss);
        return;
    }
    self.action = ^{ reply(SPUUserUpdateChoiceInstall); };
    [self show:state.stage == SPUUserUpdateStageInstalling ? Ready : Available];
}

- (void)showUpdateReleaseNotesWithDownloadData:(SPUDownloadData *)download {
    self.notes = [[NSString alloc] initWithData:download.data encoding:NSUTF8StringEncoding];
    wakeFeeder();
}

- (void)showUpdateReleaseNotesFailedToDownloadWithError:(NSError *)error {
    self.message = @"Release notes unavailable.";
    wakeFeeder();
}

- (void)showUpdateNotFoundWithError:(NSError *)error acknowledgement:(void (^)(void))acknowledge {
    NSInteger reason = [error.userInfo[SPUNoUpdateFoundReasonKey] integerValue];
    self.message = (reason == SPUNoUpdateFoundReasonOnLatestVersion || reason == SPUNoUpdateFoundReasonOnNewerThanLatestVersion)
        ? @"Feeder is up to date." : error.localizedRecoverySuggestion ?: error.localizedDescription;
    [self show:Current];
    acknowledge();
}

- (BOOL)executableReplaced {
    char live[PROC_PIDPATHINFO_MAXSIZE];
    int len = proc_pidpath(getpid(), live, sizeof(live));
    if (len <= 0) {
        return YES;
    }
    NSString *livePath = [[NSFileManager defaultManager] stringWithFileSystemRepresentation:live length:(NSUInteger)len];
    if (![NSFileManager.defaultManager fileExistsAtPath:livePath]) {
        return YES;
    }
    NSString *bundleExec = NSBundle.mainBundle.executablePath.stringByResolvingSymlinksInPath;
    if (bundleExec.length > 0 && ![livePath.stringByResolvingSymlinksInPath isEqualToString:bundleExec]) {
        return YES;
    }
    return NO;
}

- (void)showUpdaterError:(NSError *)error acknowledgement:(void (^)(void))acknowledge {
    if ([self executableReplaced]) {
        self.message = @"Feeder was replaced on disk while running. Quit and reopen Feeder.";
    } else {
        NSString *reason = error.localizedFailureReason;
        if (reason.length > 0 && ![error.localizedDescription containsString:reason]) {
            self.message = [NSString stringWithFormat:@"%@ %@", error.localizedDescription, reason];
        } else {
            self.message = error.localizedDescription;
        }
    }
    [self show:Problem];
    acknowledge();
}

- (void)showDownloadInitiatedWithCancellation:(void (^)(void))cancellation {
    self.received = 0;
    self.expected = 0;
    self.progress = -1;
    [self show:Downloading];
}

- (void)showDownloadDidReceiveExpectedContentLength:(uint64_t)length {
    self.expected = length;
}

- (void)showDownloadDidReceiveDataOfLength:(uint64_t)length {
    self.received += length;
    double progress = self.expected > 0 ? MIN(1, (double)self.received / self.expected) : -1;
    if (floor(progress * 100) != floor(self.progress * 100)) {
        self.progress = progress;
        wakeFeeder();
    }
}

- (void)showDownloadDidStartExtractingUpdate {
    self.progress = -1;
    [self show:Preparing];
}

- (void)showExtractionReceivedProgress:(double)progress {
    if (floor(progress * 100) != floor(self.progress * 100)) {
        self.progress = progress;
        wakeFeeder();
    }
}

- (void)showReadyToInstallAndRelaunch:(void (^)(SPUUserUpdateChoice))reply {
    self.action = ^{ reply(SPUUserUpdateChoiceInstall); };
    [self show:Ready];
}

- (void)showInstallingUpdateWithApplicationTerminated:(BOOL)terminated retryTerminatingApplication:(void (^)(void))retry {
    self.action = terminated ? nil : retry;
    [self show:Installing];
}

- (void)showUpdateInstalledAndRelaunched:(BOOL)relaunched acknowledgement:(void (^)(void))acknowledge {
    self.message = @"Feeder is up to date.";
    [self show:Current];
    acknowledge();
}

- (void)dismissUpdateInstallation {
    self.action = nil;
    if (self.phase != Current && self.phase != Problem && self.phase != Informational) {
        [self show:Idle];
    }
}

@end

void feeder_start_updater(void (*wake)(void)) {
    if (!NSThread.isMainThread || driver != nil) {
        return;
    }

    NSMenu *appMenu = NSApp.mainMenu.itemArray.firstObject.submenu;
    if (appMenu == nil) {
        return;
    }

    wakeFeeder = wake;
    driver = [FeederUpdateDriver new];
    updater = [[SPUUpdater alloc] initWithHostBundle:NSBundle.mainBundle applicationBundle:NSBundle.mainBundle userDriver:driver delegate:nil];
    NSError *error = nil;
    if (![updater startUpdater:&error]) {
        driver.message = error.localizedDescription;
        [driver show:Problem];
    }

    NSMenuItem *item = [[NSMenuItem alloc] initWithTitle:@"Check for Updates…"
                                                action:@selector(checkForUpdates:)
                                         keyEquivalent:@""];
    item.target = driver;
    NSInteger about = [appMenu indexOfItemWithTarget:nil andAction:@selector(orderFrontStandardAboutPanel:)];
    [appMenu insertItem:item atIndex:about < 0 ? 0 : about + 1];
}

UpdateSnapshot feeder_update_snapshot(void) {
    if (!NSThread.isMainThread || driver == nil) {
        UpdateSnapshot empty = { Idle, -1.0, NULL, NULL, NULL, false, false };
        return empty;
    }
    UpdateSnapshot snapshot = { driver.phase, driver.progress, driver.version.UTF8String, driver.notes.UTF8String,
                                driver.message.UTF8String, driver.reveal, driver.action != nil };
    driver.reveal = NO;
    return snapshot;
}

void feeder_update_advance(void) {
    if (!NSThread.isMainThread || driver == nil) {
        return;
    }
    void (^action)(void) = driver.action;
    driver.action = nil;
    if (action != nil) {
        action();
    }
}
