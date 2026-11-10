---
name: wb-android-release
description: Prepare Android builds (APK/AAB), signing and on-device acceptance
version: 1.1.0
---
# wb-android-release

**Suggested role:** `mobile-engineer`. This file is portable and does not depend on that agent existing.

## When to use
APK/AAB builds, signing, Android permissions, camera scanners or notifications.

## Procedure
1. Check variant, version, applicationId, targetSdk, signing and secrets kept out of the repository.
2. Build a reproducible artifact; record commands and hashes, never commit the keystore.
3. Test on at least two devices with different Android versions, denied/granted permissions and background states.
4. Validate notifications, scanner, offline mode, updates and persisted data.

## Output
Completed release checklist, evidence per device and blockers.
