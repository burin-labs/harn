The CLI now derives its compiled source revision from its own Git checkout or published-crate metadata,
including when another product links it without a build wrapper. An explicit revision override remains supported;
unavailable provenance produces an empty revision and a build warning instead of borrowing an unrelated checkout.
