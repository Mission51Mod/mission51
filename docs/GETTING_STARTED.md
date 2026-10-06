# Fox Studio 0.1.0: getting started

Fox Studio provides a desktop editor and the companion `fox` command-line tool for project configuration, asset inspection and mod packages. The [public source](https://github.com/Mission51Mod/mission51/tree/main/fox-studio) contains no game files.

## Start the editor

If you are building from source, follow [README-SOURCE.txt](https://github.com/Mission51Mod/mission51/blob/main/fox-studio/README-SOURCE.txt). The source folder contains no executables. With Cargo's default output directory, the Windows programs are built into `fox-studio/tools/rust/target/release`.

For a Windows portable package, extract the entire folder before opening `fox-studio.exe`. Keep `fox.exe`, `fox-tools.json` and the other supplied files together. The Windows installer installs for your user account and requires no administrator rights.

You can choose **Skip for now** in Setup to work with project files or assets you already extracted.

## Prepare your own game data

Use **Setup** when you need assets from your licensed MGSV: The Phantom Pain installation:

1. **Find the game:** select the installation Fox Studio will read.
2. **Mod test install:** select a separate copy for testing mods.
3. **Prepare game data:** choose a cache folder with enough free space, outside the game and application folders. Keep the **Metadata file** in this cache and click **Prepare game data**. Once “Local game data is ready” appears, select the archives you need and choose **Unpack**. This also creates an index.
4. **Check and finish:** review the checks and finish Setup.

The local metadata belongs to the selected game installation. It supplies the archive information needed by unpacking and mod operations. Keep it outside the mod test install. You can generate an optional name dictionary from your own archives; otherwise some names appear as hashes.

## Open or create a project

In **Project**, choose **Open…** and select a native location `project.toml` or a Fox Studio `foxproject.toml`. The card shows the selected file, root and data paths.

If the project uses paths relative to a workspace, set **Settings → Project root** to that workspace and click **Apply root**. This is the folder against which its data paths are resolved.

To start a project, choose **New project…**, pick a folder and a kind, then **Create**. A location uses the location form; mod and mission projects use the generic creator.

For a location, **Edit location** changes the project name, title, location code and ID, grid size, source biome and water level. Choose **Save location** to update its project file. Comments and other authored project sections are retained. **Reload project** rereads the saved file. Navigation with a draft offers **Save changes**, **Discard changes** or **Cancel**.

## Inspect models and textures

Open **Previews → Models**, choose **Open…** and select an extracted `.fmdl`. A project can also provide model folders in its `[preview] models` list.

Use **Frame model**, **Front**, **Side** or **Top** to orient the view. Drag with the left mouse button to orbit, the right or middle button to pan, and the wheel to zoom. **Albedo**, **Wireframe**, **Normal vectors**, **Bind bones**, **UV checker** and **Normal colours** help inspect the source. Select a mesh and **Frame selection** to examine one part. This view shows the source mesh and bind information; its visible pose label says no animation is loaded.

In **Previews → Textures**, **Open…** accepts `.ftex`, `.dds` and `.png`. Keep an FTEX's companion `.ftexs` files and extracted asset folders available. Use **RGBA/RGB/R/G/B/A** to inspect channels, **Mip** when present, and **Fit** or **1:1** for size. Project texture folders appear in its file list.

Model and texture previews are read-only. Camera, channel and display controls change the view; they do not rewrite assets.

## Save and package your work

Location edits save to the project document. App paths and appearance settings are saved separately; Windows defaults to `%APPDATA%\fox-studio\settings.json`. Uninstalling the application preserves projects and data kept outside its folder.

To export a prepared mod staging folder, open **Mods → Build a .mgsv package**. Select **Staging folder**, which must contain `metadata.xml` and your prepared mod files. Choose **Output .mgsv** with **Save as…**, then **Build .mgsv**. The output is a mod archive. Packing does not install it.

This release's Models and Textures views have no edited-model, animation or texture-image export command.

## When something does not work

| What you see | What to do |
| --- | --- |
| Empty model or texture file list | Use the view's **Open…** button, or check the project's preview folders and root. |
| Missing model textures or FTEX streams | Keep the extracted asset layout and companion files; inspect the listed unresolved material paths. |
| Unpack or mod actions need local game data | Return to Setup, select the correct game and **Metadata file**, then **Prepare game data** or **Reload metadata**. |
| Location Save reports an outside change | Reopen the document after resolving that change; keep a copy of any draft you need. |
| Build needs a native bundle | Keep the package's original executables and `fox-tools.json` together, then **Settings → Reload bundle**. Source compilation alone does not create this manifest. |
| A location generator or compatibility recipe is unavailable | Project configuration can still be edited. The public bundle does not include the private island-generation pipeline; see [CAPABILITIES.txt](https://github.com/Mission51Mod/mission51/blob/main/fox-studio/CAPABILITIES.txt). |
| A 3D preview pauses | Close the running game. The viewer also needs a Direct3D 12 or Vulkan graphics driver. |

**Next version:** animation authoring is in development for a later release. Fox Studio 0.1.0 does not export game-native animations or supply a playable Mission 51 build.
