#[cfg(windows)]
use std::env;
use std::{path::Path,
          result};

#[cfg(windows)]
use crate::{common::cli::DEFAULT_BINLINK_DIR,
            hcore::fs::FS_ROOT_PATH};
use crate::{common::ui::{UIReader,
                         UIWriter,
                         UI},
            hcore::{crypto::SigKeyPair,
                    env as henv,
                    package::ident,
                    Error::InvalidOrigin}};
#[cfg(windows)]
use std::ptr;
use url::Url;
#[cfg(windows)]
use widestring::WideCString;
#[cfg(windows)]
use winapi::shared::minwindef::LPARAM;
#[cfg(windows)]
use winapi::um::winuser::{self,
                          HWND_BROADCAST,
                          SMTO_ABORTIFHUNG,
                          WM_SETTINGCHANGE};
#[cfg(windows)]
use winreg::enums::{HKEY_LOCAL_MACHINE,
                    KEY_ALL_ACCESS,
                    KEY_READ};
#[cfg(windows)]
use winreg::RegKey;

use crate::{command,
            config,
            error::Result,
            AUTH_TOKEN_ENVVAR,
            BLDR_URL_ENVVAR,
            CTL_SECRET_ENVVAR,
            ORIGIN_ENVVAR};

pub fn start(ui: &mut UI, cache_path: &Path) -> Result<()> {
    ui.br()?;
    ui.title("Habitat CLI Setup")?;
    ui.para("Welcome to hab setup. Let's get started.")?;

    ui.heading("Habitat Builder Instance")?;
    ui.para(
        "Habitat packages can be stored in either the public builder instance \
         https://bldr.habitat.sh or in an on-premises builder depot instance. If \
         you do not set a builder URL now, the `hab` CLI will default to using \
         the public builder instance. This can be overridden at any time after setup.",
    )?;
    if ask_default_builder_instance(ui)? {
        ui.br()?;
        ui.para("Enter the url of your builder instance. The default is \
                 https://bldr.habitat.sh. The configured endpoint can be overridden any time \
                 with a `HAB_BLDR_URL` environment variable or a --url flag on the cli.")?;
        let mut url = prompt_url(ui)?;

        while valid_url(&url).is_err() {
            ui.br()?;
            ui.fatal(&format!("{}: is invalid, please provide a valid url", url))?;
            ui.br()?;

            url = prompt_url(ui)?;
        }

        write_cli_config_bldr_url(&url)?;
    } else {
        ui.br()?;
        ui.para("Alright, maybe another time. You can also set a `HAB_BLDR_URL` environment \
                 variable or pass the `--url` flag to the cli.")?;
    }

    ui.heading("Set up a default origin")?;
    ui.para("Every package in Habitat belongs to an origin, which indicates the person or \
             organization responsible for maintaining that package. Each origin also has a key \
             used to cryptographically sign packages in that origin.")?;
    ui.para("Selecting a default origin tells package building operations such as 'hab pkg \
             build' what key should be used to sign the packages produced. If you do not set a \
             default origin now, you will have to tell package building commands each time what \
             origin to use.")?;
    ui.para("For more information on origins and how they are used in building packages, please \
             consult the docs at https://www.habitat.sh/docs/create-packages-build/")?;
    if ask_default_origin(ui)? {
        ui.br()?;
        ui.para("Enter the name of your origin. If you plan to publish your packages publicly, \
                 we recommend that you select one that is not already in use on the Habitat \
                 build service found at https://bldr.habitat.sh/.")?;
        ui.para("Origins must begin with a lowercase letter or number. Allowed characters \
                 include lowercase letters, numbers, _, -. No more than 255 characters.")?;
        let mut origin = prompt_origin(ui)?;

        while !ident::is_valid_origin_name(&origin) {
            ui.br()?;
            ui.fatal(&format!("{}", InvalidOrigin(origin)))?;
            ui.br()?;

            origin = prompt_origin(ui)?;
        }
        write_cli_config_origin(&origin)?;
        ui.br()?;
        if is_origin_in_cache(&origin, cache_path) {
            ui.para(&format!("You already have an origin key for {} created and installed. \
                              Great work!",
                             &origin))?;
        } else {
            ui.heading("Create origin key pair")?;
            ui.para(&format!("It doesn't look like you have a signing key for the origin `{}'. \
                              Without it, you won't be able to build new packages successfully.",
                             &origin))?;
            ui.para("You can either create a new signing key now, or, if you are building \
                     packages for an origin that already exists, ask the owner to give you the \
                     signing key.")?;
            ui.para("For more information on the use of origin keys, please consult the \
                     documentation at https://www.habitat.sh/docs/concepts-keys/#origin-keys")?;
            if ask_create_origin(ui, &origin)? {
                create_origin(ui, &origin, cache_path)?;
            } else {
                ui.para(&format!("You might want to create an origin key later with: `hab \
                                  origin key generate {}'",
                                 &origin))?;
            }
        }
    } else {
        ui.para("Alright, maybe another time. You can also set a `HAB_ORIGIN` environment \
                 variable when using the cli.")?;
    }
    ui.heading("Habitat Personal Access Token")?;
    ui.para("While you can perform tasks like building and running Habitat packages without \
             needing to authenticate with Builder, some operations like uploading your packages \
             to Builder, or checking the status of your build jobs from the Habitat client will \
             require you to use an access token.")?;
    ui.para("The Habitat Personal Access Token can be generated via the Builder  Profile page \
             (https://bldr.habitat.sh/#/profile). Once you have generated your token, you can \
             enter it here.")?;
    ui.para("If you would like to save your token for use by the Habitat client, please enter \
             your access token. Otherwise, just enter No.")?;
    ui.para(
        "For more information on using Builder, please read the \
         documentation at https://www.habitat.sh/docs/using-builder/",
    )?;
    if ask_default_auth_token(ui)? {
        ui.br()?;
        ui.para("Enter your Habitat Personal Access Token.")?;
        let auth_token = prompt_auth_token(ui)?;
        write_cli_config_auth_token(&auth_token)?;
    } else {
        ui.para("Alright, maybe another time. You can also set a `HAB_AUTH_TOKEN` environment \
                 variable or pass the `--auth` flag to the cli.")?;
    }
    ui.heading("Supervisor Control Gateway Secret")?;
    ui.para("The Supervisor control gateway secret is used to authenticate hab client commands \
             to a Supervisor. When a new Supervisor is created, a unique secret is generated \
             automatically and stored locally in a file: `/hab/sup/default/CTL_SECRET`. When \
             issuing commands to a local Supervisor, the control gateway secret is retrieved \
             from this file. Typically, a default Supervisor control gateway secret would only \
             be needed if you wish to send commands to a remote supervisor, in which case, a \
             default control gateway secret would need to match the one already in use by the \
             remote supervisor.")?;
    if ask_default_ctl_secret(ui)? {
        ui.br()?;
        ui.para("Enter your Habitat Supervisor control gateway secret.")?;
        let ctl_secret = prompt_ctl_secret(ui)?;
        write_cli_config_ctl_secret(&ctl_secret)?;
    } else {
        ui.para("Alright, maybe another time. You can also set a `HAB_CTL_SECRET` environment \
                 variable when issuing commands to a remote Supervisor.")?;
    }
    #[cfg(windows)]
    {
        let binlink_path =
            Path::new(&*FS_ROOT_PATH).join(Path::new(DEFAULT_BINLINK_DIR).strip_prefix("/")
                                                                         .unwrap());
        ui.heading("Habitat Binlink Path")?;
        ui.para("The `hab` command-line tool can create binlinks for package binaries in the \
                 'PATH' when using the 'pkg binlink' or 'pkg install --binlink' commands. By \
                 default, Habitat will create these binlinks in the '/hab/bin' directory. This \
                 directory will always be included in your 'PATH' when inside a Studio \
                 environment. However, you will want this directory on your machine's \
                 persistent 'PATH' in order to access binlinks outside of a Studio.")?;
        if ui.prompt_yes_no("Add binlink directory to PATH?", Some(true))? {
            if !fs::am_i_root() {
                ui.fatal("You must be in an elevated console running as an administrator in \
                          order to set the machine's PATH. Please open a new console running as \
                          administrator and run setup again to add the Habitat binlink \
                          directory to your path.")?;
            } else {
                set_binlink_path(&binlink_path)?;
                ui.para(&format!("{} has been added to your path. You will need to open a new \
                                  console window for this added entry to take effect.",
                                 binlink_path.display()))?;
            }
        } else {
            ui.para("Alright, maybe another time.")?;
        }
    }
    ui.heading("CLI Setup Complete")?;
    ui.para("That's all for now. Thanks for using Habitat!")?;
    Ok(())
}

fn ask_default_origin(ui: &mut UI) -> Result<bool> {
    Ok(ui.prompt_yes_no("Set up a default origin?", Some(true))?)
}

fn ask_default_builder_instance(ui: &mut UI) -> Result<bool> {
    Ok(ui.prompt_yes_no("Connect to an on-premises Builder instance?", Some(false))?)
}

fn ask_create_origin(ui: &mut UI, origin: &str) -> Result<bool> {
    Ok(ui.prompt_yes_no(&format!("Create an origin key for `{}'?", origin),
                        Some(true))?)
}

fn write_cli_config_origin(origin: &str) -> Result<()> {
    let mut config = config::load()?;
    config.origin = Some(origin.to_string());
    config::save(&config)
}

fn write_cli_config_bldr_url(url: &str) -> Result<()> {
    let mut config = config::load()?;
    config.bldr_url = Some(url.to_string());
    config::save(&config)
}

fn write_cli_config_auth_token(auth_token: &str) -> Result<()> {
    let mut config = config::load()?;
    config.auth_token = Some(auth_token.to_string());
    config::save(&config)
}

fn write_cli_config_ctl_secret(value: &str) -> Result<()> {
    let mut config = config::load()?;
    config.ctl_secret = Some(value.to_string());
    config::save(&config)
}

fn is_origin_in_cache(origin: &str, cache_path: &Path) -> bool {
    match SigKeyPair::get_latest_pair_for(origin, cache_path, None) {
        Ok(pair) => {
            match pair.secret() {
                Ok(_) => true,
                _ => false,
            }
        }
        _ => false,
    }
}

fn create_origin(ui: &mut UI, origin: &str, cache_path: &Path) -> Result<()> {
    let result = command::origin::key::generate::start(ui, &origin, cache_path);
    ui.br()?;
    result
}

fn prompt_origin(ui: &mut UI) -> Result<String> {
    let config = config::load()?;
    let default = match config.origin {
        Some(o) => {
            ui.para(&format!("You already have a default origin set up as `{}', but feel free \
                              to change it if you wish.",
                             &o))?;
            Some(o)
        }
        None => henv::var(ORIGIN_ENVVAR).or_else(|_| henv::var("USER")).ok(),
    };
    Ok(ui.prompt_ask("Default origin name", default.as_ref().map(|x| &**x))?)
}

fn ask_default_auth_token(ui: &mut UI) -> Result<bool> {
    Ok(ui.prompt_yes_no("Set up a default Builder personal access token?",
                        Some(false))?)
}

fn ask_default_ctl_secret(ui: &mut UI) -> Result<bool> {
    Ok(ui.prompt_yes_no("Set up a default Habitat Supervisor control gateway secret?",
                        Some(false))?)
}

fn prompt_url(ui: &mut UI) -> Result<String> {
    let config = config::load()?;
    let default = match config.bldr_url {
        Some(u) => {
            ui.para("You already have a default builder url set up, but feel free to change it \
                     if you wish.")?;
            Some(u)
        }
        None => henv::var(BLDR_URL_ENVVAR).ok(),
    };
    Ok(ui.prompt_ask("Private builder url", default.as_ref().map(|x| &**x))?)
}

fn prompt_auth_token(ui: &mut UI) -> Result<String> {
    let config = config::load()?;
    let default = match config.auth_token {
        Some(o) => {
            ui.para("You already have a default auth token set up, but feel free to change it \
                     if you wish.")?;
            Some(o)
        }
        None => henv::var(AUTH_TOKEN_ENVVAR).ok(),
    };
    Ok(ui.prompt_ask("Habitat personal access token",
                     default.as_ref().map(|x| &**x))?)
}

fn prompt_ctl_secret(ui: &mut UI) -> Result<String> {
    let config = config::load()?;
    let default = match config.ctl_secret {
        Some(o) => {
            ui.para(
                    "You already have a default control gateway secret set up, but feel free to \
                     change it
                if you wish.",
            )?;
            Some(o)
        }
        None => henv::var(CTL_SECRET_ENVVAR).ok(),
    };
    Ok(ui.prompt_ask("Habitat Supervisor control gateway secret",
                     default.as_ref().map(|x| &**x))?)
}

fn valid_url(val: &str) -> result::Result<(), String> {
    match Url::parse(&val) {
        Ok(_) => Ok(()),
        Err(_) => Err(format!("URL: '{}' is not valid", &val)),
    }
}

#[cfg(windows)]
fn binlink_is_on_path(binlink_path: &Path) -> bool {
    match RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey_with_flags(
        r"System\CurrentControlSet\Control\Session Manager\Environment",
        KEY_READ,
    ) {
        Ok(env) => {
            let path: String = env
                .get_value("path")
                .expect("could not find a machine PATH");
            env::split_paths(&path).any(|p| p == binlink_path)
        }
        _ => false,
    }
}

/// this sets the permanent machine PATH and not
/// the path of this process. These are maintained
/// in the Windows registry
#[cfg(windows)]
fn set_binlink_path(binlink_path: &Path) -> Result<()> {
    if !binlink_is_on_path(binlink_path) {
        let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
        let env = hklm.open_subkey_with_flags(
            r"System\CurrentControlSet\Control\Session Manager\Environment",
            KEY_ALL_ACCESS,
        )?;
        let mut paths = vec![binlink_path.to_path_buf()];
        let path: String = env.get_value("path")?;
        paths.append(&mut env::split_paths(&path).collect());
        env.set_value("path", &env::join_paths(paths)?.to_str().unwrap())?;

        // After altering the machine environment, we must broadcast
        // a WM_SETTINGCHANGE message to all windows so the user
        // will not need to sign out/in for the new path to take effect
        unsafe {
            winuser::SendMessageTimeoutW(HWND_BROADCAST,
                                         WM_SETTINGCHANGE,
                                         0,
                                         WideCString::from_str("Environment").unwrap().as_ptr()
                                         as LPARAM,
                                         SMTO_ABORTIFHUNG,
                                         5000,
                                         ptr::null_mut());
        }
    }
    Ok(())
}
