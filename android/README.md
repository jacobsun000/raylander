# Raylander Android dock

A minimal Kotlin WebView wrapper for a dedicated landscape Android phone. It loads the existing Raylander web dashboard, enters immersive full screen, and exposes a JavaScript bridge that sets the window brightness to zero when Hyprland reports that all computer monitors are off.

## Configure

On first launch, enter the computer's LAN IP address and Raylander port. The app stores both values persistently in Android `SharedPreferences`. Long-press anywhere on the dashboard to change them later.

Raylander must listen on an address reachable from the phone, and the phone and computer must be on the same network.

## Build

Open `android/` in Android Studio, allow Gradle sync to finish, then run the `app` configuration on the phone. The included Gradle wrapper also supports command-line builds:

```sh
JAVA_HOME=/opt/android-studio/jbr ./gradlew assembleDebug
JAVA_HOME=/opt/android-studio/jbr ./gradlew installDebug
```

Any installed JDK 17 or 21 can be used instead of Android Studio's bundled JDK.

The debug APK is written to `app/build/outputs/apk/debug/app-debug.apk`.

## HyperOS setup

On Xiaomi HyperOS 1.0.10:

1. Disable battery optimization for **Raylander Dock**.
2. Enable **Autostart** if the phone should recover after reboot.
3. Lock the app in the Recents screen so HyperOS does not terminate it.
4. Keep the phone connected to power and disable any system-level scheduled battery saver.
5. If edge gestures reveal system bars, enable app pinning/kiosk behavior in HyperOS settings where available.

The app deliberately keeps its Activity awake and makes the window black with brightness `0` instead of locking Android. This keeps the WebSocket alive and permits immediate restoration. On OLED this should consume very little display power, but Android/HyperOS may clamp the actual minimum brightness above physical zero.

## Display-power protocol

Run `raylander display-power` after changing DPMS state. For example, in `hypridle.conf`:

```ini
on-timeout = hyprctl dispatch dpms off && raylander display-power
on-resume = hyprctl dispatch dpms on && raylander display-power
```

The command asks the running Raylander daemon to query Hyprland and broadcast this HTMX out-of-band element (the same state is also sent when the WebSocket first connects):

```html
<div id="display-power" data-on="false" hidden hx-swap-oob="outerHTML"></div>
```

The page observes the element and calls:

```js
RaylanderAndroid.setDisplayEnabled(false)
```

When enabled, the app removes its window-level brightness override so Android restores the dock's current system brightness. This does not modify Android's global brightness setting.
