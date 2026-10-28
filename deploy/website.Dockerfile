# The official server's website (ghcr.io/dissssy/synththing-website): the
# front page (deploy/website/index.html) and the docs rendered by mdBook
# at /docs/, served as plain files on port 80. release.yml's website job
# puts both in its build folder; the reverse proxy in front sends the
# server's own paths to the server and the rest here (docs/DEPLOY.md).

FROM caddy:2-alpine
COPY . /srv
EXPOSE 80
CMD ["caddy", "file-server", "--root", "/srv", "--listen", ":80"]
