-- Drop existing tables to allow clean recreation with new schema Types
DROP TABLE IF EXISTS ktmb_trips;
DROP TABLE IF EXISTS ktmb_calendar;
DROP TABLE IF EXISTS ktmb_routes;
DROP TABLE IF EXISTS ktmb_shapes;
DROP TABLE IF EXISTS ktmb_stops;
DROP TABLE IF EXISTS ktmb_stop_times;

DROP TABLE IF EXISTS rapid_bus_penang_trips;
DROP TABLE IF EXISTS rapid_bus_penang_calendar;
DROP TABLE IF EXISTS rapid_bus_penang_routes;
DROP TABLE IF EXISTS rapid_bus_penang_shapes;
DROP TABLE IF EXISTS rapid_bus_penang_stops;
DROP TABLE IF EXISTS rapid_bus_penang_stop_times;

DROP TABLE IF EXISTS rapid_bus_mrtfeeder_trips;
DROP TABLE IF EXISTS rapid_bus_mrtfeeder_calendar;
DROP TABLE IF EXISTS rapid_bus_mrtfeeder_routes;
DROP TABLE IF EXISTS rapid_bus_mrtfeeder_shapes;
DROP TABLE IF EXISTS rapid_bus_mrtfeeder_stops;
DROP TABLE IF EXISTS rapid_bus_mrtfeeder_stop_times;

DROP TABLE IF EXISTS rapid_rail_kl_trips;
DROP TABLE IF EXISTS rapid_rail_kl_calendar;
DROP TABLE IF EXISTS rapid_rail_kl_routes;
DROP TABLE IF EXISTS rapid_rail_kl_shapes;
DROP TABLE IF EXISTS rapid_rail_kl_stops;
DROP TABLE IF EXISTS rapid_rail_kl_stop_times;

DROP TABLE IF EXISTS rapid_bus_kl_trips;
DROP TABLE IF EXISTS rapid_bus_kl_calendar;
DROP TABLE IF EXISTS rapid_bus_kl_routes;
DROP TABLE IF EXISTS rapid_bus_kl_shapes;
DROP TABLE IF EXISTS rapid_bus_kl_stops;
DROP TABLE IF EXISTS rapid_bus_kl_stop_times;

DROP TABLE IF EXISTS mybas_johor_trips;
DROP TABLE IF EXISTS mybas_johor_calendar;
DROP TABLE IF EXISTS mybas_johor_routes;
DROP TABLE IF EXISTS mybas_johor_shapes;
DROP TABLE IF EXISTS mybas_johor_stops;
DROP TABLE IF EXISTS mybas_johor_stop_times;

DROP TABLE IF EXISTS mybas_ipoh_trips;
DROP TABLE IF EXISTS mybas_ipoh_calendar;
DROP TABLE IF EXISTS mybas_ipoh_routes;
DROP TABLE IF EXISTS mybas_ipoh_shapes;
DROP TABLE IF EXISTS mybas_ipoh_stops;
DROP TABLE IF EXISTS mybas_ipoh_stop_times;

DROP TABLE IF EXISTS trip;
DROP TABLE IF EXISTS vehicle_positions;
DROP TABLE IF EXISTS logs;

-- ktmb static API data --
CREATE TABLE IF NOT EXISTS ktmb_trips (
    route_id TEXT,
    service_id TEXT,
    trip_id VARCHAR(20) PRIMARY KEY,
    trip_headsign TEXT,
    direction_id INTEGER, -- 0 = from Jetty or 1 = end Jetty
    shape_id TEXT
    -- FOREIGN KEY (route_id) REFERENCES trips(routes),
    -- FOREIGN KEY (service_id) REFERENCES trips(calendar),
    -- FOREIGN KEY (shape_id) REFERENCES trips(shapes),
);
CREATE TABLE IF NOT EXISTS ktmb_calendar (
    service_id TEXT,
    monday BOOLEAN,
    tuesday BOOLEAN,
    wednesday BOOLEAN,
    thursday BOOLEAN,
    friday BOOLEAN,
    saturday BOOLEAN,
    sunday BOOLEAN,
    start_date INTEGER,
    end_date INTEGER,
    PRIMARY KEY (service_id, start_date, end_date)
);
CREATE TABLE IF NOT EXISTS ktmb_routes (
    route_id TEXT PRIMARY KEY,
    agency_id TEXT,
    route_short_name TEXT,
    route_long_name TEXT,
    route_type INTEGER
    -- FOREIGN KEY (agency_id) REFERENCES trips(agency),
);
CREATE TABLE IF NOT EXISTS ktmb_shapes (
    shape_id TEXT,
    shape_pt_lat REAL,
    shape_pt_lon REAL,
    shape_pt_sequence INTEGER,
    shape_dist_traveled REAL,
    PRIMARY KEY (shape_id, shape_pt_sequence)
);
CREATE TABLE IF NOT EXISTS ktmb_stops (
    stop_id TEXT PRIMARY KEY,
    stop_code TEXT,
    stop_name TEXT,
    stop_lat REAL,
    stop_lon REAL
);
CREATE TABLE IF NOT EXISTS ktmb_stop_times (
    trip_id TEXT,
    arrival_time TEXT,
    departure_time TEXT,
    stop_id TEXT,
    stop_sequence INTEGER,
    stop_headsign TEXT,
    shape_dist_traveled REAL,
    PRIMARY KEY (trip_id, stop_sequence)
    -- FOREIGN KEY (trip_id) REFERENCES trips(trip_id),
    -- FOREIGN KEY (stop_id) REFERENCES stops(stop_id)
);

-- rapid_bus_penang static API data --
CREATE TABLE IF NOT EXISTS rapid_bus_penang_trips (
    route_id TEXT,
    service_id TEXT,
    trip_id VARCHAR(20) PRIMARY KEY,
    trip_headsign TEXT,
    direction_id INTEGER, -- 0 = from Jetty or 1 = end Jetty
    shape_id TEXT
    -- FOREIGN KEY (route_id) REFERENCES trips(routes),
    -- FOREIGN KEY (service_id) REFERENCES trips(calendar),
    -- FOREIGN KEY (shape_id) REFERENCES trips(shapes),
);
CREATE TABLE IF NOT EXISTS rapid_bus_penang_calendar (
    service_id TEXT,
    monday BOOLEAN,
    tuesday BOOLEAN,
    wednesday BOOLEAN,
    thursday BOOLEAN,
    friday BOOLEAN,
    saturday BOOLEAN,
    sunday BOOLEAN,
    start_date INTEGER,
    end_date INTEGER,
    PRIMARY KEY (service_id, start_date, end_date)
);
CREATE TABLE IF NOT EXISTS rapid_bus_penang_routes (
    route_id TEXT PRIMARY KEY,
    agency_id TEXT,
    route_short_name TEXT,
    route_long_name TEXT,
    route_type INTEGER
    -- FOREIGN KEY (agency_id) REFERENCES trips(agency),
);
CREATE TABLE IF NOT EXISTS rapid_bus_penang_shapes (
    shape_id TEXT,
    shape_pt_lat REAL,
    shape_pt_lon REAL,
    shape_pt_sequence INTEGER,
    shape_dist_traveled REAL,
    PRIMARY KEY (shape_id, shape_pt_sequence)
);
CREATE TABLE IF NOT EXISTS rapid_bus_penang_stops (
    stop_id TEXT PRIMARY KEY,
    stop_code TEXT,
    stop_name TEXT,
    stop_lat REAL,
    stop_lon REAL
);
CREATE TABLE IF NOT EXISTS rapid_bus_penang_stop_times (
    trip_id TEXT,
    arrival_time TEXT,
    departure_time TEXT,
    stop_id TEXT,
    stop_sequence INTEGER,
    stop_headsign TEXT,
    shape_dist_traveled REAL,
    PRIMARY KEY (trip_id, stop_sequence)
    -- FOREIGN KEY (trip_id) REFERENCES trips(trip_id),
    -- FOREIGN KEY (stop_id) REFERENCES stops(stop_id)
);

-- rapid_bus_mrtfeeder static API data --
CREATE TABLE IF NOT EXISTS rapid_bus_mrtfeeder_trips (
    route_id TEXT,
    service_id TEXT,
    trip_id VARCHAR(20) PRIMARY KEY,
    trip_headsign TEXT,
    direction_id INTEGER, -- 0 = from Jetty or 1 = end Jetty
    shape_id TEXT
    -- FOREIGN KEY (route_id) REFERENCES trips(routes),
    -- FOREIGN KEY (service_id) REFERENCES trips(calendar),
    -- FOREIGN KEY (shape_id) REFERENCES trips(shapes),
);
CREATE TABLE IF NOT EXISTS rapid_bus_mrtfeeder_calendar (
    service_id TEXT,
    monday BOOLEAN,
    tuesday BOOLEAN,
    wednesday BOOLEAN,
    thursday BOOLEAN,
    friday BOOLEAN,
    saturday BOOLEAN,
    sunday BOOLEAN,
    start_date INTEGER,
    end_date INTEGER,
    PRIMARY KEY (service_id, start_date, end_date)
);
CREATE TABLE IF NOT EXISTS rapid_bus_mrtfeeder_routes (
    route_id TEXT PRIMARY KEY,
    agency_id TEXT,
    route_short_name TEXT,
    route_long_name TEXT,
    route_type INTEGER
    -- FOREIGN KEY (agency_id) REFERENCES trips(agency),
);
CREATE TABLE IF NOT EXISTS rapid_bus_mrtfeeder_shapes (
    shape_id TEXT,
    shape_pt_lat REAL,
    shape_pt_lon REAL,
    shape_pt_sequence INTEGER,
    shape_dist_traveled REAL,
    PRIMARY KEY (shape_id, shape_pt_sequence)
);
CREATE TABLE IF NOT EXISTS rapid_bus_mrtfeeder_stops (
    stop_id TEXT PRIMARY KEY,
    stop_code TEXT,
    stop_name TEXT,
    stop_lat REAL,
    stop_lon REAL
);
CREATE TABLE IF NOT EXISTS rapid_bus_mrtfeeder_stop_times (
    trip_id TEXT,
    arrival_time TEXT,
    departure_time TEXT,
    stop_id TEXT,
    stop_sequence INTEGER,
    stop_headsign TEXT,
    shape_dist_traveled REAL,
    PRIMARY KEY (trip_id, stop_sequence)
    -- FOREIGN KEY (trip_id) REFERENCES trips(trip_id),
    -- FOREIGN KEY (stop_id) REFERENCES stops(stop_id)
);

-- rapid_rail_kl static API data --
CREATE TABLE IF NOT EXISTS rapid_rail_kl_trips (
    route_id TEXT,
    service_id TEXT,
    trip_id VARCHAR(20) PRIMARY KEY,
    trip_headsign TEXT,
    direction_id INTEGER, -- 0 = from Jetty or 1 = end Jetty
    shape_id TEXT
    -- FOREIGN KEY (route_id) REFERENCES trips(routes),
    -- FOREIGN KEY (service_id) REFERENCES trips(calendar),
    -- FOREIGN KEY (shape_id) REFERENCES trips(shapes),
);
CREATE TABLE IF NOT EXISTS rapid_rail_kl_calendar (
    service_id TEXT,
    monday BOOLEAN,
    tuesday BOOLEAN,
    wednesday BOOLEAN,
    thursday BOOLEAN,
    friday BOOLEAN,
    saturday BOOLEAN,
    sunday BOOLEAN,
    start_date INTEGER,
    end_date INTEGER,
    PRIMARY KEY (service_id, start_date, end_date)
);
CREATE TABLE IF NOT EXISTS rapid_rail_kl_routes (
    route_id TEXT PRIMARY KEY,
    agency_id TEXT,
    route_short_name TEXT,
    route_long_name TEXT,
    route_type INTEGER
    -- FOREIGN KEY (agency_id) REFERENCES trips(agency),
);
CREATE TABLE IF NOT EXISTS rapid_rail_kl_shapes (
    shape_id TEXT,
    shape_pt_lat REAL,
    shape_pt_lon REAL,
    shape_pt_sequence INTEGER,
    shape_dist_traveled REAL,
    PRIMARY KEY (shape_id, shape_pt_sequence)
);
CREATE TABLE IF NOT EXISTS rapid_rail_kl_stops (
    stop_id TEXT PRIMARY KEY,
    stop_code TEXT,
    stop_name TEXT,
    stop_lat REAL,
    stop_lon REAL
);
CREATE TABLE IF NOT EXISTS rapid_rail_kl_stop_times (
    trip_id TEXT,
    arrival_time TEXT,
    departure_time TEXT,
    stop_id TEXT,
    stop_sequence INTEGER,
    stop_headsign TEXT,
    shape_dist_traveled REAL,
    PRIMARY KEY (trip_id, stop_sequence)
    -- FOREIGN KEY (trip_id) REFERENCES trips(trip_id),
    -- FOREIGN KEY (stop_id) REFERENCES stops(stop_id)
);

-- rapid_bus_kl static API data --
CREATE TABLE IF NOT EXISTS rapid_bus_kl_trips (
    route_id TEXT,
    service_id TEXT,
    trip_id VARCHAR(20) PRIMARY KEY,
    trip_headsign TEXT,
    direction_id INTEGER, -- 0 = from Jetty or 1 = end Jetty
    shape_id TEXT
    -- FOREIGN KEY (route_id) REFERENCES trips(routes),
    -- FOREIGN KEY (service_id) REFERENCES trips(calendar),
    -- FOREIGN KEY (shape_id) REFERENCES trips(shapes),
);
CREATE TABLE IF NOT EXISTS rapid_bus_kl_calendar (
    service_id TEXT,
    monday BOOLEAN,
    tuesday BOOLEAN,
    wednesday BOOLEAN,
    thursday BOOLEAN,
    friday BOOLEAN,
    saturday BOOLEAN,
    sunday BOOLEAN,
    start_date INTEGER,
    end_date INTEGER,
    PRIMARY KEY (service_id, start_date, end_date)
);
CREATE TABLE IF NOT EXISTS rapid_bus_kl_routes (
    route_id TEXT PRIMARY KEY,
    agency_id TEXT,
    route_short_name TEXT,
    route_long_name TEXT,
    route_type INTEGER
    -- FOREIGN KEY (agency_id) REFERENCES trips(agency),
);
CREATE TABLE IF NOT EXISTS rapid_bus_kl_shapes (
    shape_id TEXT,
    shape_pt_lat REAL,
    shape_pt_lon REAL,
    shape_pt_sequence INTEGER,
    shape_dist_traveled REAL,
    PRIMARY KEY (shape_id, shape_pt_sequence)
);
CREATE TABLE IF NOT EXISTS rapid_bus_kl_stops (
    stop_id TEXT PRIMARY KEY,
    stop_code TEXT,
    stop_name TEXT,
    stop_lat REAL,
    stop_lon REAL
);
CREATE TABLE IF NOT EXISTS rapid_bus_kl_stop_times (
    trip_id TEXT,
    arrival_time TEXT,
    departure_time TEXT,
    stop_id TEXT,
    stop_sequence INTEGER,
    stop_headsign TEXT,
    PRIMARY KEY (trip_id, stop_sequence)
    -- FOREIGN KEY (trip_id) REFERENCES trips(trip_id),
    -- FOREIGN KEY (stop_id) REFERENCES stops(stop_id)
);

-- mybas_johor static API data --
CREATE TABLE IF NOT EXISTS mybas_johor_trips (
    route_id TEXT,
    service_id TEXT,
    trip_id VARCHAR(20) PRIMARY KEY,
    trip_headsign TEXT,
    direction_id INTEGER, -- 0 = from Jetty or 1 = end Jetty
    shape_id TEXT,
    block_id TEXT,
    wheelchair_accessible INTEGER
    -- FOREIGN KEY (route_id) REFERENCES trips(routes),
    -- FOREIGN KEY (service_id) REFERENCES trips(calendar),
    -- FOREIGN KEY (shape_id) REFERENCES trips(shapes),
);
CREATE TABLE IF NOT EXISTS mybas_johor_calendar (
    service_id TEXT,
    monday BOOLEAN,
    tuesday BOOLEAN,
    wednesday BOOLEAN,
    thursday BOOLEAN,
    friday BOOLEAN,
    saturday BOOLEAN,
    sunday BOOLEAN,
    start_date INTEGER,
    end_date INTEGER,
    PRIMARY KEY (service_id, start_date, end_date)
);
CREATE TABLE IF NOT EXISTS mybas_johor_routes (
    route_id TEXT PRIMARY KEY,
    agency_id TEXT,
    route_short_name TEXT,
    route_long_name TEXT,
    route_type INTEGER,
    route_desc TEXT,
    route_url TEXT,
    route_color TEXT,
    route_text_color TEXT
    -- FOREIGN KEY (agency_id) REFERENCES trips(agency),
);
CREATE TABLE IF NOT EXISTS mybas_johor_shapes (
    shape_id TEXT,
    shape_pt_lat REAL,
    shape_pt_lon REAL,
    shape_pt_sequence INTEGER,
    shape_dist_traveled REAL,
    PRIMARY KEY (shape_id, shape_pt_sequence)
);
CREATE TABLE IF NOT EXISTS mybas_johor_stops (
    stop_id TEXT PRIMARY KEY,
    stop_code TEXT,
    stop_name TEXT,
    stop_lat REAL,
    stop_lon REAL,
    stop_desc TEXT,
    zone_id TEXT,
    stop_url TEXT,
    location_type INTEGER,
    parent_station TEXT
);
CREATE TABLE IF NOT EXISTS mybas_johor_stop_times (
    trip_id TEXT,
    arrival_time TEXT,
    departure_time TEXT,
    stop_id TEXT,
    stop_sequence INTEGER,
    stop_headsign TEXT,
    shape_dist_traveled REAL,
    pickup_type INTEGER,
    drop_off_type INTEGER,
    PRIMARY KEY (trip_id, stop_sequence)
    -- FOREIGN KEY (trip_id) REFERENCES trips(trip_id),
    -- FOREIGN KEY (stop_id) REFERENCES stops(stop_id)
);

-- mybas_ipoh static API data --
CREATE TABLE IF NOT EXISTS mybas_ipoh_trips (
    route_id TEXT,
    service_id TEXT,
    trip_id VARCHAR(20) PRIMARY KEY,
    direction_id INTEGER, -- 0 = from Jetty or 1 = end Jetty
    shape_id TEXT
    -- FOREIGN KEY (route_id) REFERENCES trips(routes),
    -- FOREIGN KEY (service_id) REFERENCES trips(calendar),
    -- FOREIGN KEY (shape_id) REFERENCES trips(shapes),
);
CREATE TABLE IF NOT EXISTS mybas_ipoh_calendar (
    service_id TEXT,
    monday BOOLEAN,
    tuesday BOOLEAN,
    wednesday BOOLEAN,
    thursday BOOLEAN,
    friday BOOLEAN,
    saturday BOOLEAN,
    sunday BOOLEAN,
    start_date INTEGER,
    end_date INTEGER,
    PRIMARY KEY (service_id, start_date, end_date)
);
CREATE TABLE IF NOT EXISTS mybas_ipoh_routes (
    route_id TEXT PRIMARY KEY,
    agency_id TEXT,
    route_short_name TEXT,
    route_long_name TEXT,
    route_type INTEGER
    -- FOREIGN KEY (agency_id) REFERENCES trips(agency),
);
CREATE TABLE IF NOT EXISTS mybas_ipoh_shapes (
    shape_id TEXT,
    shape_pt_lat REAL,
    shape_pt_lon REAL,
    shape_pt_sequence INTEGER,
    PRIMARY KEY (shape_id, shape_pt_sequence)
);
CREATE TABLE IF NOT EXISTS mybas_ipoh_stops (
    stop_id TEXT PRIMARY KEY,
    stop_code TEXT,
    stop_name TEXT,
    stop_lat REAL,
    stop_lon REAL,
    parent_station TEXT
);
CREATE TABLE IF NOT EXISTS mybas_ipoh_stop_times (
    trip_id TEXT,
    arrival_time TEXT,
    departure_time TEXT,
    stop_id TEXT,
    stop_sequence INTEGER,
    pickup_type INTEGER,
    drop_off_type INTEGER,
    PRIMARY KEY (trip_id, stop_sequence)
    -- FOREIGN KEY (trip_id) REFERENCES trips(trip_id),
    -- FOREIGN KEY (stop_id) REFERENCES stops(stop_id)
);

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

-- Table to store execution logs
CREATE TABLE IF NOT EXISTS logs (
    Id INTEGER PRIMARY KEY AUTOINCREMENT,
    Level TINYINT NOT NULL CHECK(Level IN (0, 1, 2, 3, 4, 5)), -- 0 = Trace, 1 = Debug, 2 = Info, 3 = Warning, 4 = Error, 5 = Critical
    Message TEXT NOT NULL,
    Timestamp DATETIME DEFAULT CURRENT_TIMESTAMP
);