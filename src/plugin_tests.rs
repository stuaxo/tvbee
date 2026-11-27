//! Tests of core with the plugins' parts installed (apps, providers,
//! pairings): what only the whole program has.

mod search {
    use tveee_core::builtin::search::*;

    const OUT: &str = "
    com.android.cts.priv.ctsshim/.InstallPriority
    com.android.vending/com.google.android.finsky.tvmainactivity.TvMainActivity
    org.xbmc.kodi/.XBMCSearchableActivity
    org.droidtv.zapster/.playpvr.activity.RecPlayActivity2
    org.droidtv.zapster/.playtv.activity.SearchActivity
    org.droidtv.zapster/.playtv.activity.EPGSearchActivity
    com.mxtech.videoplayer.pro/.ActivityMediaList
    com.mxtech.videoplayer.pro/.TVActivityMediaList
    org.videolan.vlc/.StartActivity
    com.google.android.tv.frameworkpackagestubs/.Stubs$MediaStub
@@
    org.xbmc.kodi/.XBMCSearchableActivity
    com.example.voice/.Search
@@
    org.videolan.vlc/.StartActivity
";

    #[test]
    fn finds_searchable_apps() {
        crate::plugins::install_all();
        let pm = "package:com.google.android.youtube.tv\npackage:org.videolan.vlc\n";
        let all = parse_searchable(OUT, Some(pm));
        let find = |p: &str| all.iter().find(|s| s.package == p).cloned();
        assert!(find("com.google.android.gms").is_none() && find("com.android.cts.priv.ctsshim").is_none());
        // the activity named for search wins in apps with several
        assert_eq!(
            find("org.droidtv.zapster").unwrap().way,
            Way::Intent { action: SEARCH, component: "org.droidtv.zapster/.playtv.activity.SearchActivity".into() }
        );
        assert_eq!(
            find("com.mxtech.videoplayer.pro").unwrap().way,
            Way::Intent { action: SEARCH, component: "com.mxtech.videoplayer.pro/.TVActivityMediaList".into() }
        );
        assert_eq!(find("org.videolan.vlc").unwrap().play.as_deref(), Some("org.videolan.vlc/.StartActivity"));
        assert_eq!(find("com.example.voice").unwrap().way, Way::Intent { action: GMS_SEARCH, component: "com.example.voice/.Search".into() });
        // YouTube by link, only because it's installed
        assert_eq!(find("com.google.android.youtube.tv").unwrap().way, Way::Link("https://www.youtube.com/results?search_query={q}"));
        assert!(parse_searchable(OUT, Some("")).iter().all(|s| s.package != "com.google.android.youtube.tv"));
    }

    #[test]
    fn builds_commands() {
        crate::plugins::install_all();
        let all = parse_searchable(OUT, Some("package:com.google.android.youtube.tv\n"));
        let get = |p: &str| all.iter().find(|s| s.package == p).unwrap();
        assert_eq!(
            search_command(get("org.xbmc.kodi"), "star trek", false).unwrap(),
            "am start -a android.intent.action.SEARCH -n 'org.xbmc.kodi/.XBMCSearchableActivity' --es query 'star trek'"
        );
        assert_eq!(
            search_command(get("com.google.android.youtube.tv"), "cats & dogs", false).unwrap(),
            "am start -a android.intent.action.VIEW -d 'https://www.youtube.com/results?search_query=cats%20%26%20dogs' -p 'com.google.android.youtube.tv'"
        );
        assert_eq!(
            search_command(get("org.videolan.vlc"), "it's jazz", true).unwrap(),
            "am start -a android.media.action.MEDIA_PLAY_FROM_SEARCH -n 'org.videolan.vlc/.StartActivity' --es query 'it'\\''s jazz'"
        );
        assert!(search_command(get("org.xbmc.kodi"), "x", true).is_err());
        assert_eq!(url_encode("a b/c?é"), "a%20b%2Fc%3F%C3%A9");
        // a real plus stays a plus, encoded
        assert_eq!(url_encode("c++"), "c%2B%2B");
    }
}

mod registries {
    #[test]
    fn plugin_apps_by_package() {
        crate::plugins::install_all();
        use tveee_core::builtin::apps::*;
        assert_eq!(aliases_for_package("org.droidtv.nettvapp4340"), vec!["iplayer"]);
        assert!(aliases_for_package("org.droidtv.nettvbrowser").is_empty());
        assert_eq!(by_package("com.google.android.youtube.tv").unwrap().name, "YouTube");
        assert_eq!(by_package("org.droidtv.zapster").unwrap().kind, Kind::LiveTv);
        assert_eq!(pkg_of("iplayer"), "org.droidtv.nettvapp4340");
    }

    #[test]
    fn lists_installed_aliases() {
        crate::plugins::install_all();
        let pm = "package:com.google.android.youtube.tv\npackage:org.videolan.vlc\npackage:org.droidtv.zapster\npackage:com.nowhere\n";
        let names: Vec<_> = tveee_core::builtin::apps::installed_aliases(pm).iter().filter_map(|a| a.alias).collect();
        // home is always there; the rest only if installed
        // plugin apps (livetv from philips, youtube) come right after home
        assert_eq!(names, vec!["home", "livetv", "youtube", "vlc"]);
    }

    #[test]
    fn registry_and_errors() {
        crate::plugins::install_all();
        assert!(tveee_core::pairing::by_id("jointspace").is_some());
        let e = tveee_core::pairing::required_error("jointspace", "the channel list").to_string();
        assert!(e.starts_with("the channel list needs tveee to be paired with") && e.contains("tveee pair jointspace"), "{e}");
    }

    #[test]
    fn registry_has_the_built_in_providers() {
        crate::plugins::install_all();
        use tveee_core::provider::*;
        let ids: Vec<_> = all().iter().map(|p| p.id()).collect();
        assert!(ids.contains(&"youtube") && ids.contains(&"freeview"), "{ids:?}");
        let yt = for_package("com.google.android.youtube.tv").unwrap();
        assert_eq!(capabilities(yt), vec!["details", "places", "search", "sessions", "player"]);
        let tv = with_guide(None).unwrap();
        assert_eq!(tv.id(), "freeview");
        assert_eq!(capabilities(tv), vec!["guide", "channels"]);
        assert!(with_guide(Some("youtube")).is_none());
    }
}
