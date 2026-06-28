- Windows PowerShell installer: `irm https://harnlang.com/install.ps1 | iex` downloads, checksum-verifies, and
  installs the Windows release archive and adds the install directory to the user PATH. The POSIX `install.sh`
  now points Windows shells at it instead of erroring.
