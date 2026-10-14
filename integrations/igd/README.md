# Box internet (UPnP IGD)

- UPnP standard : SSDP puis SOAP (`WANIPConnection`, `WANCommonInterfaceConfig`), toutes les 10 s.
- Volontairement sans débit instantané : les compteurs UPnP des box sont faux.
- Origine : rien de noté. Pas de fixture : les tests convertissent des débits écrits dans le
  code ; le parsing UPnP est testé dans `crates/moli-net/src/upnp.rs`.
- Vérifié sur une Livebox, avec `location` fixée (l'URL que HA connaissait). Elle ne répond à la
  recherche générique que pour un autre service (`WFADevice`, port 1990) : sa passerelle n'est
  connue que par ses annonces, d'où l'écoute. Elle change d'UDN et de chemin à chaque redémarrage :
  identité = numéro de série.
