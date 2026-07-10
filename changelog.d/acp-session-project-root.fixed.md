ACP sessions now prepare VM project-root state from the session cwd/workspace
anchor instead of leaking the server process working directory when clients
connect from another project.
