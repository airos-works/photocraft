#!/bin/sh
signature=application/x-vnd.storytold-PhotoCraft
app=$(findpaths -p "$0" B_FIND_PATH_APPS_DIRECTORY PhotoCraft 2>/dev/null)
[ -n "$app" ] && [ -e "$app" ] && mimeset -a -f "$app"
setmime -set application/x-photocraft -short "PhotoCraft document" -extension pcraft
for type in application/x-photocraft image/vnd.adobe.photoshop; do
    setmime -dump "$type" 2>/dev/null | grep -q -- -preferredAppSig \
        || setmime -set "$type" -preferredAppSig "$signature"
done
exit 0
