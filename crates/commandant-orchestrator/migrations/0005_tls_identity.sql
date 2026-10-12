-- The server's TLS certificate and its private key (PEM). Links pin the
-- certificate, so it is made once and kept for as long as the database.
CREATE TABLE tls_identity (
    id          INTEGER PRIMARY KEY CHECK (id = 1),
    certificate TEXT NOT NULL,
    key         TEXT NOT NULL
);
