# tide static file

Static file server implementation, work with [Tide](https://github.com/rustasync/tide)

# Feature

+ Whole file response
+ Single range
+ Multi ranges

# TODO

+ ETAG
+ Last-Modified
+ Content-Disposition (Non-ASCII support)
+ If-Range
+ Better performance (async file IO or 'sendfile')
+ Index file support(e.g.: index.html)
+ File list for directory (default off)
+ Merge ranges(if overlap)
+ Integration tests