Confined commands retain a caller's populated Gradle cache inside the workspace
instead of dropping `GRADLE_USER_HOME` and trying to download it again. External
cache paths relocate inside the workspace without adding filesystem authority.
