# Assets

| File | What |
|---|---|
| `icon.png` | The app icon, 1024 × 1024, transparent outside the rounded square. The master copy. |
| `tessera.ico` | The icon at 16, 20, 24, 32, 40, 48, 64, 128 and 256 px, made from `icon.png`. Sizes up to 64 are 32-bit bitmaps, 128 and 256 are PNG. The build embeds it in all three executables (`build-support/resources.rs`), which is where the tray icon, the settings window and the installer dialogs take it from. |
| `icon-64.png` | 64 px copy shown in the settings window's sidebar and About page. |

`icon.png` was cut from the original artwork on a white background. The white connected to the image border was removed, and the anti-aliased edge pixels were un-mixed from white so the corners stay smooth on any background.

To change the icon, replace `icon.png` and regenerate `tessera.ico` and `icon-64.png` at the sizes above, scaling down in halving steps for clean small sizes. Then rebuild.
