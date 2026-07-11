import urllib.request
import urllib.error
import json
import sys
import time

BASE_URL = "http://127.0.0.1:8787"

def wait_for_server():
    print("Waiting for wrangler dev server to start...")
    for _ in range(30):
        try:
            # We hit a known path just to test server is up
            urllib.request.urlopen(f"{BASE_URL}/", timeout=1)
            print("Server is up!")
            return True
        except urllib.error.URLError:
            time.sleep(1)
    print("Server failed to start.")
    return False

def test_data_api():
    provider = "ktmb"
    valid_table = "import_progress"
    invalid_table = "this_table_does_not_exist"
    
    print("\n--- Running Integration Tests ---")
    
    # 1. Test Valid Table
    try:
        url = f"{BASE_URL}/{provider}/data/{valid_table}?limit=2&offset=0"
        print(f"Testing valid table: GET {url}")
        req = urllib.request.urlopen(url)
        res = req.read().decode('utf-8')
        data = json.loads(res)
        
        # Check structure
        assert "data" in data, "Response should contain 'data' array"
        assert "limit" in data, "Response should contain 'limit'"
        assert "offset" in data, "Response should contain 'offset'"
        assert data["limit"] == 2, "Limit should be 2"
        assert data["offset"] == 0, "Offset should be 0"
        print("✅ Valid table test passed!")
    except urllib.error.HTTPError as e:
        print(f"❌ Valid table test failed: Expected 200 OK, got {e.code}")
        sys.exit(1)
    except AssertionError as e:
        print(f"❌ Valid table test failed: {e}")
        sys.exit(1)

    # 2. Test Invalid Table
    try:
        url = f"{BASE_URL}/{provider}/data/{invalid_table}"
        print(f"Testing invalid table: GET {url}")
        req = urllib.request.urlopen(url)
        print("❌ Invalid table test failed: Expected 404 Not Found, got 200 OK")
        sys.exit(1)
    except urllib.error.HTTPError as e:
        if e.code == 404:
            print("✅ Invalid table test passed (got 404)")
        else:
            print(f"❌ Invalid table test failed: Expected 404 Not Found, got {e.code}")
            sys.exit(1)

    # 3. Test Include Column Selection
    try:
        url = f"{BASE_URL}/{provider}/data/{valid_table}?include=Provider,Status&limit=2"
        print(f"Testing include selection: GET {url}")
        req = urllib.request.urlopen(url)
        res = req.read().decode('utf-8')
        data = json.loads(res)
        
        assert "data" in data, "Response should contain 'data' array"
        if len(data["data"]) > 0:
            first_row = data["data"][0]
            assert "Provider" in first_row, "Should contain 'Provider' column"
            assert "Status" in first_row, "Should contain 'Status' column"
            assert "FileName" not in first_row, "Should NOT contain 'FileName' column"
        print("✅ Include selection test passed!")
    except Exception as e:
        print(f"❌ Include selection test failed: {e}")
        sys.exit(1)

    # 4. Test Exclude Column Selection
    try:
        url = f"{BASE_URL}/{provider}/data/{valid_table}?exclude=FileName&limit=2"
        print(f"Testing exclude selection: GET {url}")
        req = urllib.request.urlopen(url)
        res = req.read().decode('utf-8')
        data = json.loads(res)
        
        assert "data" in data, "Response should contain 'data' array"
        if len(data["data"]) > 0:
            first_row = data["data"][0]
            assert "Provider" in first_row, "Should contain 'Provider' column"
            assert "FileName" not in first_row, "Should NOT contain 'FileName' column"
        print("✅ Exclude selection test passed!")
    except Exception as e:
        print(f"❌ Exclude selection test failed: {e}")
        sys.exit(1)

    # 5. Test Exact Filtering
    try:
        url = f"{BASE_URL}/{provider}/data/{valid_table}?filter=1@Status"
        print(f"Testing filtering: GET {url}")
        req = urllib.request.urlopen(url)
        res = req.read().decode('utf-8')
        data = json.loads(res)
        
        assert "data" in data, "Response should contain 'data' array"
        for row in data["data"]:
            assert str(row.get("Status")) == "1", "All rows should have Status=1"
        print("✅ Exact filtering test passed!")
    except Exception as e:
        print(f"❌ Exact filtering test failed: {e}")
        sys.exit(1)

    # 6. Test icontains Filtering
    try:
        url = f"{BASE_URL}/{provider}/data/{valid_table}?icontains=theR.zi@FileName"
        print(f"Testing icontains: GET {url}")
        req = urllib.request.urlopen(url)
        res = req.read().decode('utf-8')
        data = json.loads(res)
        
        assert "data" in data, "Response should contain 'data' array"
        for row in data["data"]:
            assert "ther.zi" in row.get("FileName", "").lower(), "FileName should contain 'ther.zi'"
        print("✅ icontains test passed!")
    except Exception as e:
        print(f"❌ icontains test failed: {e}")
        sys.exit(1)

    # 7. Test Sorting
    try:
        url = f"{BASE_URL}/{provider}/data/{valid_table}?sort=-Status"
        print(f"Testing sorting: GET {url}")
        req = urllib.request.urlopen(url)
        res = req.read().decode('utf-8')
        data = json.loads(res)
        
        assert "data" in data, "Response should contain 'data' array"
        if len(data["data"]) > 1:
            assert data["data"][0]["Status"] >= data["data"][1]["Status"], "Status should be descending"
        print("✅ Sorting test passed!")
    except Exception as e:
        print(f"❌ Sorting test failed: {e}")
        sys.exit(1)

    # 8. Test Range Filtering
    try:
        url = f"{BASE_URL}/{provider}/data/{valid_table}?range=Status[0:1]"
        print(f"Testing range: GET {url}")
        req = urllib.request.urlopen(url)
        res = req.read().decode('utf-8')
        data = json.loads(res)
        
        assert "data" in data, "Response should contain 'data' array"
        for row in data["data"]:
            assert row.get("Status") in [0, 1], "Status should be between 0 and 1"
        print("✅ Range test passed!")
    except Exception as e:
        print(f"❌ Range test failed: {e}")
        sys.exit(1)

if __name__ == "__main__":
    if wait_for_server():
        test_data_api()
        print("\nAll tests passed! 🎉")
    else:
        sys.exit(1)
