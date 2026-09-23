-- realtime API data --
CREATE TABLE IF NOT EXISTS trip (
    TripId VARCHAR(20) PRIMARY KEY,
    RouteId VARCHAR(6) NOT NULL,
    VehicleId VARCHAR(10) NOT NULL
);

CREATE TABLE IF NOT EXISTS vehiclePositions (
    TripId VARCHAR(20) NOT NULL,
    Latitude FLOAT NOT NULL,
    Longitude FLOAT NOT NULL,
    Bearing INTEGER NOT NULL,
    Speed FLOAT NOT NULL,
    Timestamp INTEGER NOT NULL
    -- FOREIGN KEY (TripId) REFERENCES Trip(TripId)
);