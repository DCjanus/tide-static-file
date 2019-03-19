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

# TODO

+ If-Unmodified-Since
+ If-Match
+ Content-Disposition (Non-ASCII support)
+ Better performance (async file IO or 'sendfile')
+ Index file support(e.g.: index.html)
+ File list for directory (default off)
+ Merge ranges(if overlap)
+ Integration tests
+ Auto check with CI
+ Better error message
+ More config
+ Code coverage report with ci

# Thanks

Learned so much excellent code from: 

+ [actix-web](https://github.com/actix/actix-web) ([LICENSE FILE](./origin-license/ACTIX-WEB-LICENSE-MIT))
+ [tomhoule/tide-static-files](https://github.com/tomhoule/tide-static-files)

Thanks for everyone!