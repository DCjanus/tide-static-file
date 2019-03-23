# tide static file

Static file server implementation, work with [Tide](https://github.com/rustasync/tide)

# Feature

+ Whole file response
+ Single range
+ Multi ranges
+ ETAG
+ Last-Modified
+ If-Range
+ If-Modified-Since
+ If-None-Match
+ If-Unmodified-Since
+ If-Match
+ Content-Disposition (Non-ASCII support)
+ Merge ranges(if overlap)

# TODO

+ Better performance (async file IO or 'sendfile')
+ Index file support(e.g., index.html)
+ File list for directory (default off)
+ Integration tests
+ Auto check with CI
+ Better error message
+ More config
+ Code coverage report with ci
+ Percent encoding( e.g., Chinese filename)

# Thanks

Learned so much excellent code from: 

+ [actix-web](https://github.com/actix/actix-web) ([LICENSE FILE](./origin-license/ACTIX-WEB-LICENSE-MIT))
+ [tomhoule/tide-static-files](https://github.com/tomhoule/tide-static-files)

Thanks for everyone!