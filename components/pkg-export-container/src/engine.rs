use clap::{Arg,
           ArgMatches};
use habitat_core::fs::find_command;
use std::{fmt,
          process::Command,
          result::Result,
          str::FromStr};

/// Things that can build containers!
#[derive(Clone, Copy, Debug)]
pub enum Engine {
    Docker,
    #[cfg(not(windows))]
    Buildah,
}

// really, want as_str

impl fmt::Display for Engine {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        match self {
            Engine::Docker => write!(f, "docker"),
            #[cfg(not(windows))]
            Engine::Buildah => write!(f, "buildah"),
        }
    }
}

impl FromStr for Engine {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "docker" => Ok(Engine::Docker),
            #[cfg(not(windows))]
            "buildah" => Ok(Engine::Buildah),
            _ => Err("LOLWUT".to_string()),
        }
    }
}

impl Engine {
    /// Returns a `Command` pointing to the binary that implements the
    /// chosen container creation engine.
    pub fn command(self) -> Command {
        let cmd_path =
            find_command(&self.to_string()).expect("Could not find engine command on path!");
        Command::new(cmd_path)
    }

    // TODO (CM): is Engine actually a Trait of ImageBuilder?

    // TODO (CM): might need Engine to be a trait... put the various
    // commands behind a facade that deals with their differences

    /// Return the engine-specific subcommand for building a container
    /// image.
    pub fn build_subcommand(self) -> &'static str {
        match self {
            Self::Docker => "build",
            #[cfg(not(windows))]
            Self::Buildah => "build-using-dockerfile",
        }
    }

    /// Define the CLAP CLI argument for specifying a container build
    /// engine to use.
#[rustfmt::skip] // otherwise the long_help formatting goes crazy
    pub fn cli_arg<'a, 'b>() -> Arg<'a, 'b> {
        let arg =
            Arg::with_name("ENGINE").value_name("ENGINE")
                                    .long("engine")
                                    .required(true)
                                    .takes_value(true)
                                    .multiple(false)
                                    .default_value("docker")
                                    .validator(Engine::valid_engine)
                                    .help("The name of the container creation engine to use.");
        // TODO (CM): Find a way to tie this more closely to the
        // Engine enum values.
        if cfg!(windows) {
            // Since there is effectively no choice of engine for
            // Windows, we hide the CLI option and don't document it
            // any further.
            arg.possible_values(&["docker"]).hidden(true)
        } else {
            arg.long_help(
"Using the `docker` engine allows you to use Docker to create
your container images. You must ensure that a Docker daemon
is running on the host where this command is executed, and
that the user executing the command has permission to access
the Docker socket.

Using the `buildah` engine allows you to create container images
as an unprivileged user, and without having to use a Docker
daemon. This is the recommended engine for use in CI systems and
other environments where security is of particular concern.
Please see https://buildah.io for more details.

Both engines create equivalent OCI-compliant container images.
",
            )
               .possible_values(&["docker", "buildah"])
        }
    }

    /// CLAP validator function
    #[allow(clippy::needless_pass_by_value)] // Signature required by CLAP
    fn valid_engine(val: String) -> std::result::Result<(), String> {
        val.parse::<Engine>().map(|_| ())
    }

    /// Constructs a valid `Engine` from CLI arguments.
    pub fn new_from_cli_matches(m: &ArgMatches) -> Engine {
        clap::value_t!(m.value_of("ENGINE"), Engine).expect("engine is a required option")
    }
}
